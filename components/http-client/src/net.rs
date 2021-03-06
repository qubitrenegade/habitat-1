use std::io::{Read,
              Write};

use httparse;
use hyper::{self,
            method::Method,
            net::{HttpConnector,
                  HttpsStream,
                  NetworkConnector,
                  SslClient},
            version::HttpVersion};

use crate::proxy::ProxyInfo;

/// A connector that uses an HTTP proxy server (pass-through for plaintext and tunneled for SSL
/// sessions).
pub struct ProxyHttpsConnector<S: SslClient> {
    proxy:           ProxyInfo,
    proxy_connector: HttpConnector,
    ssl_client:      S,
}

impl<S: SslClient> ProxyHttpsConnector<S> {
    /// Creates a new connection using the provided proxy server configuration and SSL
    /// implementation.
    pub fn new(proxy: ProxyInfo, ssl_client: S) -> hyper::Result<Self> {
        Ok(ProxyHttpsConnector { proxy,
                                 proxy_connector: HttpConnector,
                                 ssl_client })
    }
}

impl<S: SslClient> NetworkConnector for ProxyHttpsConnector<S> {
    type Stream = HttpsStream<S::Stream>;

    fn connect(&self, host: &str, port: u16, scheme: &str) -> hyper::Result<Self::Stream> {
        // Initial connection to the proxy server, using an `HttpConnector`
        let mut stream = self.proxy_connector
                             .connect(self.proxy.host(), self.proxy.port(), "http")?;
        match scheme {
            "https" => {
                // If the target URL is an `"https"` scheme, then we use proxy/TCP tunneling as
                // per the RFC draft:
                // http://www.web-cache.com/Writings/Internet-Drafts/
                // draft-luotonen-web-proxy-tunneling-01.txt
                //
                // We can't yet use hyper directly and therefore use the underlying http parsing
                // library to establish the connection and parse the response. This implementation
                // is largely based on hyper's internal proxy tunneling code.
                let mut connect_msg = format!("{method} {host}:{port} {version}\r\nHost: \
                                               {host}:{port}\r\n",
                                              method = Method::Connect,
                                              version = HttpVersion::Http11,
                                              host = host,
                                              port = port);
                if let Some(header_value) = self.proxy.authorization_header_value() {
                    connect_msg.push_str(&format!("Proxy-Authorization: {}\r\n", header_value));
                };
                connect_msg.push_str("\r\n");
                debug!("Proxy {}:{} {:?}",
                       self.proxy.host(),
                       self.proxy.port(),
                       connect_msg.trim().replace("\r\n", ", "));
                stream.write_all(connect_msg.as_bytes())?;
                stream.flush()?;
                let mut buf = [0; 1024];
                let mut n = 0;
                while n < buf.len() {
                    n += stream.read(&mut buf[n..])?;
                    let mut headers = [httparse::EMPTY_HEADER; 10];
                    let mut res = httparse::Response::new(&mut headers);
                    if res.parse(&buf[..n])?.is_complete() {
                        let code = res.code.expect("complete parsing lost code");
                        if code >= 200 && code < 300 {
                            debug!("Proxy {}:{} CONNECT success = {}",
                                   self.proxy.host(),
                                   self.proxy.port(),
                                   code);
                            return self.ssl_client
                                       .wrap_client(stream, host)
                                       .map(HttpsStream::Https);
                        } else {
                            debug!("Proxy {}:{} CONNECT failed response = {}",
                                   self.proxy.host(),
                                   self.proxy.port(),
                                   code);
                            return Err(hyper::Error::Status);
                        }
                    }
                }
                Err(hyper::Error::TooLarge)
            }
            _ => Ok(HttpsStream::Http(stream)),
        }
    }
}
