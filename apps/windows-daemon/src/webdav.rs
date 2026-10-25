use dav_server::DavHandler;
use dav_server::davpath::DavPath;
use dav_server::fs::*;
use futures_util::future::{BoxFuture, FutureExt};
use futures_util::stream::{BoxStream, StreamExt};
use hyper::{
    Server,
    service::{make_service_fn, service_fn},
};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, oneshot};

use crate::UsbJob;
use crate::windroid::packet::Payload;
use crate::windroid::{DirectoryRequest, FileReadRequest, Packet};

#[derive(Clone, Debug)]
pub struct AndroidFs {
    pub tx: mpsc::Sender<UsbJob>,
}

impl AndroidFs {
    async fn send_req(&self, packet: Packet) -> Result<Packet, FsError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(UsbJob {
                packet,
                reply: reply_tx,
            })
            .await
            .map_err(|_| FsError::GeneralFailure)?;

        reply_rx
            .await
            .map_err(|_| FsError::GeneralFailure)?
            .map_err(|_| FsError::GeneralFailure)
    }
}

impl DavFileSystem for AndroidFs {
    fn open<'a>(
        &'a self,
        path: &'a DavPath,
        _options: OpenOptions,
    ) -> BoxFuture<'a, FsResult<Box<dyn DavFile>>> {
        async move {
            let p = path.as_url_string();
            Ok(Box::new(AndroidFile {
                fs: self.clone(),
                path: p,
                offset: 0,
            }) as Box<dyn DavFile>)
        }
        .boxed()
    }

    fn read_dir<'a>(
        &'a self,
        path: &'a DavPath,
        _meta: ReadDirMeta,
    ) -> BoxFuture<'a, FsResult<BoxStream<'static, Box<dyn DavDirEntry>>>> {
        async move {
            let mut p = path.as_url_string();
            if p.is_empty() {
                p = "/".to_string();
            }

            let req = Packet {
                payload: Some(Payload::DirRequest(DirectoryRequest { path: p })),
            };
            let resp = self.send_req(req).await?;

            if let Some(Payload::DirResponse(dir_resp)) = resp.payload {
                if dir_resp.success {
                    let mut entries: Vec<Box<dyn DavDirEntry>> = Vec::new();
                    for file in dir_resp.files {
                        entries.push(Box::new(AndroidDirEntry { file }));
                    }
                    let stream = futures_util::stream::iter(entries);
                    Ok(stream.boxed())
                } else {
                    Err(FsError::NotFound)
                }
            } else {
                Err(FsError::GeneralFailure)
            }
        }
        .boxed()
    }

    fn metadata<'a>(&'a self, path: &'a DavPath) -> BoxFuture<'a, FsResult<Box<dyn DavMetaData>>> {
        async move {
            let p = path.as_url_string();
            if p.is_empty() || p == "/" {
                return Ok(Box::new(AndroidMetaData {
                    size: 0,
                    is_dir: true,
                    modified: 0,
                }) as Box<dyn DavMetaData>);
            }

            let parent = path.parent();
            let parent_str = if parent.as_url_string().is_empty() {
                "/".to_string()
            } else {
                parent.as_url_string()
            };

            let file_name = p
                .split('/')
                .filter(|s| !s.is_empty())
                .last()
                .unwrap_or("")
                .to_string();

            let req = Packet {
                payload: Some(Payload::DirRequest(DirectoryRequest { path: parent_str })),
            };
            let resp = self.send_req(req).await?;

            if let Some(Payload::DirResponse(dir_resp)) = resp.payload {
                if let Some(f) = dir_resp.files.into_iter().find(|f| f.name == file_name) {
                    return Ok(Box::new(AndroidMetaData {
                        size: f.size_bytes as u64,
                        is_dir: f.is_directory,
                        modified: f.last_modified_ms as u64,
                    }) as Box<dyn DavMetaData>);
                }
            }
            Err(FsError::NotFound)
        }
        .boxed()
    }
}

#[derive(Clone, Debug)]
struct AndroidMetaData {
    size: u64,
    is_dir: bool,
    modified: u64,
}

impl DavMetaData for AndroidMetaData {
    fn len(&self) -> u64 {
        self.size
    }
    fn is_dir(&self) -> bool {
        self.is_dir
    }
    fn modified(&self) -> FsResult<SystemTime> {
        Ok(SystemTime::UNIX_EPOCH + Duration::from_millis(self.modified))
    }
}

#[derive(Debug)]
struct AndroidDirEntry {
    file: crate::windroid::FileInfo,
}

impl DavDirEntry for AndroidDirEntry {
    fn name(&self) -> Vec<u8> {
        self.file.name.as_bytes().to_vec()
    }
    fn metadata(&self) -> BoxFuture<'_, FsResult<Box<dyn DavMetaData>>> {
        async move {
            Ok(Box::new(AndroidMetaData {
                size: self.file.size_bytes as u64,
                is_dir: self.file.is_directory,
                modified: self.file.last_modified_ms as u64,
            }) as Box<dyn DavMetaData>)
        }
        .boxed()
    }
}

#[derive(Debug)]
struct AndroidFile {
    fs: AndroidFs,
    path: String,
    offset: u64,
}

impl DavFile for AndroidFile {
    fn read_bytes(&mut self, count: usize) -> BoxFuture<'_, FsResult<bytes::Bytes>> {
        let offset = self.offset;
        self.offset += count as u64;

        let path = self.path.clone();
        let fs = self.fs.clone();

        async move {
            let req = Packet {
                payload: Some(Payload::FileReq(FileReadRequest {
                    path,
                    offset: offset as i64,
                    max_bytes: count as i32,
                })),
            };
            let resp = fs.send_req(req).await?;
            if let Some(Payload::FileRes(f_resp)) = resp.payload {
                if f_resp.success {
                    Ok(bytes::Bytes::from(f_resp.data))
                } else {
                    Err(FsError::GeneralFailure)
                }
            } else {
                Err(FsError::GeneralFailure)
            }
        }
        .boxed()
    }

    fn metadata(&mut self) -> BoxFuture<'_, FsResult<Box<dyn DavMetaData>>> {
        async move { Err(FsError::NotImplemented) }.boxed()
    }

    fn write_buf(&mut self, _buf: Box<dyn bytes::Buf + Send>) -> BoxFuture<'_, FsResult<()>> {
        async move { Err(FsError::NotImplemented) }.boxed()
    }

    fn write_bytes(&mut self, _buf: bytes::Bytes) -> BoxFuture<'_, FsResult<()>> {
        async move { Err(FsError::NotImplemented) }.boxed()
    }

    fn seek(&mut self, pos: std::io::SeekFrom) -> BoxFuture<'_, FsResult<u64>> {
        let new_offset = match pos {
            std::io::SeekFrom::Start(off) => off,
            std::io::SeekFrom::Current(diff) => (self.offset as i64 + diff) as u64,
            std::io::SeekFrom::End(_) => {
                return async move { Err(FsError::NotImplemented) }.boxed();
            }
        };
        self.offset = new_offset;
        async move { Ok(new_offset) }.boxed()
    }

    fn flush(&mut self) -> BoxFuture<'_, FsResult<()>> {
        async move { Ok(()) }.boxed()
    }
}

pub async fn serve_webdav(tx: mpsc::Sender<UsbJob>) {
    let fs = AndroidFs { tx };
    let dav_server = DavHandler::builder()
        .filesystem(Box::new(fs))
        .build_handler();

    let make_service = make_service_fn(move |_| {
        let dav_server = dav_server.clone();
        async move {
            let func = move |req| {
                let dav_server = dav_server.clone();
                async move { Ok::<_, Infallible>(dav_server.handle(req).await) }
            };
            Ok::<_, Infallible>(service_fn(func))
        }
    });

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let server = Server::bind(&addr).serve(make_service);
    if let Err(e) = server.await {
        eprintln!("WebDAV server error: {}", e);
    }
}
