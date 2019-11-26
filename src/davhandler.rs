//
// This module contains the main entry point of the library,
// DavHandler.
//
use std::error::Error as StdError;
use std::io;
use std::sync::Arc;

use bytes::{self, Bytes, buf::Buf, buf::FromBuf, buf::IntoBuf};
use futures::stream::{Stream, StreamExt, TryStreamExt};
use headers::HeaderMapExt;
use http::{Request, Response, StatusCode};

use crate::body::{Body, InBody};
use crate::davheaders;
use crate::util::{dav_method, AllowedMethods, Method};
use crate::davpath::DavPath;

use crate::errors::DavError;
use crate::fs::*;
use crate::ls::*;
use crate::voidfs::{VoidFs, is_voidfs};
use crate::DavResult;

/// The webdav handler struct.
///
/// The `new` and `build` etc methods are used to instantiate a handler.
///
/// The `handle` and `handle_with` methods are the methods that do the actual work.
#[derive(Clone)]
pub struct DavHandler {
    config: Arc<DavConfig>,
}

/// Configuration of the handler.
#[derive(Default)]
pub struct DavConfig {
    /// Prefix to be stripped off when handling request.
    pub prefix: Option<String>,
    /// Filesystem backend.
    pub fs: Option<Box<dyn DavFileSystem>>,
    /// Locksystem backend.
    pub ls: Option<Box<dyn DavLockSystem>>,
    /// Set of allowed methods (None means "all methods")
    pub allow: Option<AllowedMethods>,
    /// Principal is webdav speak for "user", used to give locks an owner (if a locksystem is
    /// active).
    pub principal: Option<String>,
    /// Hide symbolic links? `None` maps to `true`.
    pub hide_symlinks: Option<bool>,
}

impl DavConfig {
    /// Create a new configuration builder.
    pub fn new() -> DavConfig {
        DavConfig::default()
    }

    /// Use the configuration that was built to generate a DavConfig.
    pub fn build_handler(self) -> DavHandler {
        DavHandler{ config: Arc::new(self) }
    }

    /// Prefix to be stripped off before translating the rest of
    /// the request path to a filesystem path.
    pub fn strip_prefix(self, prefix: String) -> Self {
        let mut this = self;
        this.prefix = Some(prefix);
        this
    }

    /// Set the filesystem to use.
    pub fn filesystem(self, fs: Box<dyn DavFileSystem>) -> Self {
        let mut this = self;
        this.fs = Some(fs);
        this
    }

    /// Set the locksystem to use.
    pub fn locksystem(self, ls: Box<dyn DavLockSystem>) -> Self {
        let mut this = self;
        this.ls = Some(ls);
        this
    }

    /// Which methods to allow (default is all methods).
    pub fn methods(self, allow: AllowedMethods) -> Self {
        let mut this = self;
        this.allow = Some(allow);
        this
    }

    /// Set the name of the "webdav principal". This will be the owner of any created locks.
    pub fn principal(self, principal: String) -> Self {
        let mut this = self;
        this.principal = Some(principal);
        this
    }

    /// Hide symbolic links (default is true)
    pub fn hide_symlinks(self, hide: bool) -> Self {
        let mut this = self;
        this.hide_symlinks = Some(hide);
        this
    }

    fn merge(&self, new: DavConfig) -> DavConfig {
        DavConfig {
            prefix:        new.prefix.or(self.prefix.clone()),
            fs:            new.fs.or(self.fs.clone()),
            ls:            new.ls.or(self.ls.clone()),
            allow:         new.allow.or(self.allow.clone()),
            principal:     new.principal.or(self.principal.clone()),
            hide_symlinks: new.hide_symlinks.or(self.hide_symlinks.clone()),
        }
    }
}

// The actual inner struct.
//
// At the start of the request, DavConfig is used to generate
// a DavInner struct. DavInner::handle then handles the request.
pub(crate) struct DavInner {
    pub prefix:        String,
    pub fs:            Box<dyn DavFileSystem>,
    pub ls:            Option<Box<dyn DavLockSystem>>,
    pub allow:         Option<AllowedMethods>,
    pub principal:     Option<String>,
    pub hide_symlinks: Option<bool>,
}

impl From<DavConfig> for DavInner {
    fn from(cfg: DavConfig) -> Self {
        DavInner {
            prefix:        cfg.prefix.unwrap_or("".to_string()),
            fs:            cfg.fs.unwrap_or(VoidFs::new()),
            ls:            cfg.ls,
            allow:         cfg.allow,
            principal:     cfg.principal,
            hide_symlinks: cfg.hide_symlinks,
        }
    }
}

impl From<&DavConfig> for DavInner {
    fn from(cfg: &DavConfig) -> Self {
        DavInner {
            prefix:        cfg
                .prefix
                .as_ref()
                .map(|p| p.to_owned())
                .unwrap_or("".to_string()),
            fs:            cfg.fs.clone().unwrap(),
            ls:            cfg.ls.clone(),
            allow:         cfg.allow,
            principal:     cfg.principal.clone(),
            hide_symlinks: cfg.hide_symlinks.clone(),
        }
    }
}

impl Clone for DavInner {
    fn clone(&self) -> Self {
        DavInner {
            prefix:        self.prefix.clone(),
            fs:            self.fs.clone(),
            ls:            self.ls.clone(),
            allow:         self.allow.clone(),
            principal:     self.principal.clone(),
            hide_symlinks: self.hide_symlinks.clone(),
        }
    }
}

impl DavHandler {
    /// Create a new `DavHandler`.
    ///
    /// This returns a DavHandler with an empty configuration. That's only
    /// useful if you use the `handle_with` method instead of `handle`.
    /// Normally you should create a new `DavHandler` using `DavHandler::build`
    /// and configure at least the filesystem, and probably the strip_prefix.
    pub fn new() -> DavHandler {
        DavHandler{ config: Arc::new(DavConfig::default()) }
    }

    /// Return a configuration builder.
    pub fn builder() -> DavConfig {
        DavConfig::new()
    }

    /// Handle a webdav request.
    ///
    /// Only one error kind is ever returned: `ErrorKind::BrokenPipe`. In that case we
    /// were not able to generate a response at all, and the server should just
    /// close the connection.
    pub async fn handle<ReqBody, ReqData, ReqError>(
        &self,
        req: Request<ReqBody>,
    ) -> io::Result<Response<Body>>
    where
        ReqData: Buf + Send,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: http_body::Body<Data = ReqData, Error = ReqError> + Send + Unpin,
    {
        let (req, body) = {
            let (parts, body) = req.into_parts();
            (Request::from_parts(parts, ()), InBody::from(body))
        };
        let inner = DavInner::from(&*self.config);
        inner.handle(req, body).await
    }

    /// Handle a webdav request, overriding parts of the config.
    ///
    /// For example, the `principal` can be set for this request.
    ///
    /// Or, the default config has no locksystem, and you pass in
    /// a fake locksystem (`FakeLs`) because this is a request from a
    /// windows or macos client that needs to see locking support.
    pub async fn handle_with<ReqBody, ReqData, ReqError>(
        &self,
        config: DavConfig,
        req: Request<ReqBody>,
    ) -> io::Result<Response<Body>>
    where
        ReqData: Buf + Send,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: http_body::Body<Data = ReqData, Error = ReqError> + Send + Unpin,
    {
        let (req, body) = {
            let (parts, body) = req.into_parts();
            (Request::from_parts(parts, ()), InBody::from(body))
        };
        let inner = DavInner::from(self.config.merge(config));
        inner.handle(req, body).await
    }

    /// Handles a request with a `Stream` body instead of a `http_body::Body`.
    /// Used with webserver frameworks that have not
    /// opted to use the `http_body` crate just yet.
    #[doc(hidden)]
    pub async fn handle_stream<ReqBody, ReqData, ReqError>(
        &self,
        req: Request<ReqBody>,
    ) -> io::Result<Response<Body>>
    where
        ReqData: IntoBuf + Send,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: Stream<Item = Result<ReqData, ReqError>> + Send + Unpin,
    {
        let (req, body) = {
            let (parts, body) = req.into_parts();
            (Request::from_parts(parts, ()), body.map_ok(|buf| Bytes::from_buf(buf.into_buf())))
        };
        let inner = DavInner::from(&*self.config);
        inner.handle(req, body).await
    }

    /// Handles a request with a `Stream` body instead of a `http_body::Body`.
    #[doc(hidden)]
    pub async fn handle_stream_with<ReqBody, ReqData, ReqError>(
        &self,
        config: DavConfig,
        req: Request<ReqBody>,
    ) -> io::Result<Response<Body>>
    where
        ReqData: IntoBuf + Send,
        ReqError: StdError + Send + Sync + 'static,
        ReqBody: Stream<Item = Result<ReqData, ReqError>> + Send + Unpin,
    {
        let (req, body) = {
            let (parts, body) = req.into_parts();
            (Request::from_parts(parts, ()), body.map_ok(|buf| Bytes::from_buf(buf.into_buf())))
        };
        let inner = DavInner::from(self.config.merge(config));
        inner.handle(req, body).await
    }
}

impl DavInner {
    // helper.
    pub(crate) async fn has_parent<'a>(&'a self, path: &'a DavPath) -> bool {
        let p = path.parent();
        self.fs.metadata(&p).await.map(|m| m.is_dir()).unwrap_or(false)
    }

    // helper.
    pub(crate) fn path(&self, req: &Request<()>) -> DavPath {
        // This never fails (has been checked before)
        DavPath::from_uri(req.uri(), &self.prefix).unwrap()
    }

    // See if this is a directory and if so, if we have
    // to fixup the path by adding a slash at the end.
    pub(crate) fn fixpath(
        &self,
        res: &mut Response<Body>,
        path: &mut DavPath,
        meta: Box<dyn DavMetaData>,
    ) -> Box<dyn DavMetaData>
    {
        if meta.is_dir() && !path.is_collection() {
            path.add_slash();
            let newloc = path.with_prefix().as_url_string();
            res.headers_mut()
                .typed_insert(davheaders::ContentLocation(newloc));
        }
        meta
    }

    // drain request body and return length.
    pub(crate) async fn read_request<'a, ReqBody, ReqError>(
        &'a self,
        body: ReqBody,
        max_size: usize,
    ) -> DavResult<Vec<u8>>
    where
        ReqBody: Stream<Item = Result<Bytes, ReqError>> + Send + 'a,
        ReqError: StdError + Send + Sync + 'static,
    {
        let mut data = Vec::new();
        pin_utils::pin_mut!(body);
        while let Some(res) = body.next().await {
            let chunk = res.map_err(|_| {
                DavError::IoError(io::Error::new(io::ErrorKind::UnexpectedEof, "UnexpectedEof"))
            })?;
            if data.len() + chunk.len() > max_size {
                return Err(StatusCode::PAYLOAD_TOO_LARGE.into());
            }
            data.extend_from_slice(&chunk);
        }
        Ok(data)
    }

    // internal dispatcher.
    async fn handle<ReqBody, ReqError>(self, req: Request<()>, body: ReqBody) -> io::Result<Response<Body>>
    where
        ReqBody: Stream<Item = Result<Bytes, ReqError>> + Send + Unpin,
        ReqError: StdError + Send + Sync + 'static,
    {
        let is_ms = req
            .headers()
            .get("user-agent")
            .and_then(|s| s.to_str().ok())
            .map(|s| s.contains("Microsoft"))
            .unwrap_or(false);

        // Turn any DavError results into a HTTP error response.
        match self.handle2(req, body).await {
            Ok(resp) => {
                debug!("== END REQUEST result OK");
                Ok(resp)
            },
            Err(err) => {
                debug!("== END REQUEST result {:?}", err);
                let mut resp = Response::builder();
                if is_ms && err.statuscode() == StatusCode::NOT_FOUND {
                    // This is an attempt to convince Windows to not
                    // cache a 404 NOT_FOUND for 30-60 seconds.
                    //
                    // That is a problem since windows caches the NOT_FOUND in a
                    // case-insensitive way. So if "www" does not exist, but "WWW" does,
                    // and you do a "dir www" and then a "dir WWW" the second one
                    // will fail.
                    //
                    // Ofcourse the below is not sufficient. Fixes welcome.
                    resp.header("Cache-Control", "no-store, no-cache, must-revalidate");
                    resp.header("Progma", "no-cache");
                    resp.header("Expires", "0");
                    resp.header("Vary", "*");
                }
                resp.header("Content-Length", "0");
                resp.status(err.statuscode());
                if err.must_close() {
                    resp.header("connection", "close");
                }
                let resp = resp.body(Body::empty()).unwrap();
                Ok(resp)
            },
        }
    }

    // internal dispatcher part 2.
    async fn handle2<ReqBody, ReqError>(mut self, req: Request<()>, body: ReqBody) -> DavResult<Response<Body>>
    where
        ReqBody: Stream<Item = Result<Bytes, ReqError>> + Send + Unpin,
        ReqError: StdError + Send + Sync + 'static,
    {
        // debug when running the webdav litmus tests.
        if log_enabled!(log::Level::Debug) {
            if let Some(t) = req.headers().typed_get::<davheaders::XLitmus>() {
                debug!("X-Litmus: {:?}", t);
            }
        }

        // translate HTTP method to Webdav method.
        let method = match dav_method(req.method()) {
            Ok(m) => m,
            Err(e) => {
                debug!("refusing method {} request {}", req.method(), req.uri());
                return Err(e);
            },
        };

        // See if method makes sense if we do not have a fileystem.
        if is_voidfs(&self.fs) {
            match method {
                Method::Options => {
                    if self.allow.as_ref().map(|a| a.allowed(Method::Options)).unwrap_or(true) {
                        let mut a = AllowedMethods::none();
                        a.add(Method::Options);
                        self.allow = Some(a);
                    }
                },
                _ => {
                    debug!("no filesystem: method not allowed on request {}", req.uri());
                    return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
                }
            }
        }

        // see if method is allowed.
        if let Some(ref a) = self.allow {
            if !a.allowed(method) {
                debug!("method {} not allowed on request {}", req.method(), req.uri());
                return Err(DavError::StatusClose(StatusCode::METHOD_NOT_ALLOWED));
            }
        }

        // make sure the request path is valid.
        let path = DavPath::from_uri(req.uri(), &self.prefix)?;

        // PUT is the only handler that reads the body itself. All the
        // other handlers either expected no body, or a pre-read Vec<u8>.
        let (body_strm, body_data) = match method {
            Method::Put | Method::Patch => (Some(body), Vec::new()),
            _ => (None, self.read_request(body, 65536).await?),
        };

        // Not all methods accept a body.
        match method {
            Method::Put | Method::Patch | Method::PropFind | Method::PropPatch | Method::Lock => {},
            _ => {
                if body_data.len() > 0 {
                    return Err(StatusCode::UNSUPPORTED_MEDIA_TYPE.into());
                }
            },
        }

        debug!("== START REQUEST {:?} {}", method, path);

        let res = match method {
            Method::Options => self.handle_options(&req).await,
            Method::PropFind => self.handle_propfind(&req, &body_data).await,
            Method::PropPatch => self.handle_proppatch(&req,&body_data).await,
            Method::MkCol => self.handle_mkcol(&req).await,
            Method::Delete => self.handle_delete(&req).await,
            Method::Lock => self.handle_lock(&req, &body_data).await,
            Method::Unlock => self.handle_unlock(&req).await,
            Method::Head | Method::Get => self.handle_get(&req).await,
            Method::Put | Method::Patch => self.handle_put(&req, &mut body_strm.unwrap()).await,
            Method::Copy | Method::Move => self.handle_copymove(&req, method).await,
        };
        res
    }
}
