
use http::{Request, Response, StatusCode};

use futures::prelude::*;
use futures03::compat::Future01CompatExt;
use futures03::future::Future as Future03;

use crate::{DavError, DavResult, RespData};
use crate::asyncstream::AsyncStream;
use crate::conditional::*;
use crate::fs::*;
use crate::headers;
use crate::typed_headers::HeaderMapExt;

impl crate::DavInner {

    pub(crate) fn handle_mkcol(self, req: Request<()>)
    -> impl Future03<Output=DavResult<Response<RespData>>>
    {
        async move {

            let mut path = self.path(&req);
            let meta = self.fs.metadata(&path);

            // check the If and If-* headers.
            let res = await!(if_match_get_tokens(&req, meta.as_ref().ok(), &self.fs, &self.ls, &path));
            let tokens = match res {
                Ok(t) => t,
                Err(s) => return Err(DavError::Status(s)),
            };

            // if locked check if we hold that lock.
            if let Some(ref locksystem) = self.ls {
                let t = tokens.iter().map(|s| s.as_str()).collect::<Vec<&str>>();
                let principal = self.principal.as_ref().map(|s| s.as_str());
                if let Err(_l) = locksystem.check(&path, principal, false, false, t) {
                    return Err(DavError::Status(StatusCode::LOCKED));
                }
            }

            let mut res = Response::new(AsyncStream::empty());

            match blocking_io!(self.fs.create_dir(&path)) {
                // RFC 4918 9.3.1 MKCOL Status Codes.
                Err(FsError::Exists) => return Err(DavError::Status(StatusCode::METHOD_NOT_ALLOWED)),
                Err(FsError::NotFound) => return Err(DavError::Status(StatusCode::CONFLICT)),
                Err(e) => return Err(DavError::FsError(e)),
                Ok(()) => {
                    if path.is_collection() {
                        path.add_slash();
                        res.headers_mut().typed_insert(headers::ContentLocation(path.as_url_string_with_prefix()));
                    }
                    *res.status_mut() = StatusCode::CREATED;
                }
            }

            Ok(res)
        }
    }
}

