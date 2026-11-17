//! GPUI image transport over Threadlane's existing HTTP stack and reactor.
//! The upstream adapter requires a separate reqwest fork; keep one HTTP stack.
use std::time::Duration;

use futures::{future::BoxFuture, AsyncReadExt};
use gpui::http_client::{
    http::HeaderValue, AsyncBody, HttpClient, RedirectPolicy, Request, RequestTimeout, Response,
    Url,
};

pub(super) struct ImageHttpClient {
    client: reqwest::Client,
}

impl ImageHttpClient {
    pub(super) fn new() -> gpui::Result<Self> {
        Ok(Self {
            client: Self::builder().build()?,
        })
    }

    fn builder() -> reqwest::ClientBuilder {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::limited(100))
    }
}

impl HttpClient for ImageHttpClient {
    fn user_agent(&self) -> Option<&HeaderValue> {
        None
    }
    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: Request<AsyncBody>,
    ) -> BoxFuture<'static, gpui::Result<Response<AsyncBody>>> {
        let client = self.client.clone();
        Box::pin(async move {
            let (parts, mut body) = request.into_parts();
            let client = match parts.extensions.get::<RedirectPolicy>() {
                Some(RedirectPolicy::NoFollow) | None => Self::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?,
                Some(RedirectPolicy::FollowLimit(limit)) => Self::builder()
                    .redirect(reqwest::redirect::Policy::limited(*limit as usize))
                    .build()?,
                Some(RedirectPolicy::FollowAll) => client,
            };
            let mut bytes = Vec::new();
            body.read_to_end(&mut bytes).await?;
            let mut request = client
                .request(parts.method, parts.uri.to_string())
                .headers(parts.headers)
                .body(bytes);
            if let Some(timeout) = parts.extensions.get::<RequestTimeout>() {
                request = request.timeout(timeout.0);
            }
            // GPUI's asset executor has no Tokio reactor. Keep both the request
            // and body read on the shared reactor, then return an in-memory body.
            let runtime =
                threadlane_provider::exec::try_get_runtime().map_err(std::io::Error::other)?;
            runtime
                .spawn(async move {
                    let response = request.send().await?;
                    let mut builder = Response::builder()
                        .status(response.status())
                        .version(response.version());
                    *builder.headers_mut().expect("valid response builder") =
                        response.headers().clone();
                    let body = response.bytes().await?;
                    Ok(builder.body(body.into())?)
                })
                .await?
        })
    }
}

#[cfg(test)]
mod tests {
    use super::ImageHttpClient;
    use futures::AsyncReadExt;
    use gpui::http_client::HttpClient;
    use std::{
        io::{BufRead, BufReader, Write},
        net::TcpListener,
        time::{Duration, Instant},
    };

    #[test]
    fn image_fetch_uses_shared_reactor_and_honors_redirect_policy() {
        const SVG: &str = "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"1\" height=\"1\"><rect width=\"1\" height=\"1\"/></svg>";
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let deadline = Instant::now() + Duration::from_secs(5);
            for path in ["/redirect", "/redirect", "/image.svg"] {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "image request did not reach local server"
                            );
                            std::thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("{error}"),
                    }
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap();
                let mut request = String::new();
                BufReader::new(&mut stream).read_line(&mut request).unwrap();
                assert!(request.starts_with(&format!("GET {path} ")));
                if path == "/redirect" {
                    write!(stream, "HTTP/1.1 302 Found\r\nLocation: /image.svg\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
                } else {
                    write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/svg+xml\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{SVG}", SVG.len()).unwrap();
                }
            }
        });
        let client = ImageHttpClient::new().unwrap();
        // Deliberately poll from an ordinary thread, as GPUI does.
        futures::executor::block_on(async {
            let url = format!("http://{address}/redirect");
            let response = client.get(&url, ().into(), false).await.unwrap();
            assert_eq!(response.status().as_u16(), 302);
            let mut response = client.get(&url, ().into(), true).await.unwrap();
            assert_eq!(response.status().as_u16(), 200);
            assert_eq!(response.headers()["content-type"], "image/svg+xml");
            let mut body = String::new();
            response.body_mut().read_to_string(&mut body).await.unwrap();
            assert_eq!(body, SVG);
        });
        server.join().unwrap();
    }
}
