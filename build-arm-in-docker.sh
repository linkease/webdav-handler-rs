#!/bin/bash

#alias rust-musl-builder='docker run --rm -it -v "$(pwd)":/home/rust/src messense/rust-musl-cross:armv7-musleabi'
docker run --rm -it -v "$(pwd)":/home/rust/src messense/rust-musl-cross:armv7-musleabi cargo build --example sample-litmus-server
docker run --rm -it -v "$(pwd)":/home/rust/src messense/rust-musl-cross:armv7-musleabi musl-strip target/armv7-unknown-linux-musleabi/debug/examples/sample-litmus-server
