//! Modbus TCP server skeleton

use crate::{
    codec,
    frame::*,
    server::service::{NewService, Service},
};

use futures::{self, Future};
use futures_util::{future::FutureExt as _, sink::SinkExt as _, stream::StreamExt as _};
use log::{error, trace};
use socket2::{Domain, Socket, Type};
use std::{
    io::{self, Error},
    net::SocketAddr,
    sync::Arc,
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::codec::Framed;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Server {
    socket_addr: SocketAddr,
}

impl Server {
    /// Set the address for the server (mandatory).
    pub fn new(socket_addr: SocketAddr) -> Self {
        Self { socket_addr }
    }

    /// Start an async Modbus TCP server task.
    pub async fn serve<S>(&self, service: S) -> Result<(), std::io::Error>
    where
        S: NewService<Request = Request, Response = Response> + Send + Sync + 'static,
        S::Request: From<Request>,
        S::Response: Into<Response>,
        S::Error: Into<Error>,
        S::Instance: Send + Sync + 'static,
    {
        let service = Arc::new(service);
        let sock_addr = self.socket_addr;
        let listener = TcpListener::bind(sock_addr).await?;

        loop {
            log::debug!("Listening for requests");
            let (stream, addr) = listener.accept().await?;
            log::debug!("Accepted incoming request from {:?}", addr);

            let framed = Framed::new(stream, codec::tcp::ServerCodec::default());
            let new_service = service.clone();

            tokio::spawn(Box::pin(async move {
                let service = new_service.new_service().unwrap();
                if let Err(err) = process(sock_addr, framed, service).await {
                    log::error!("{:?}", err);
                }
            }));
        }
    }

    /// Start a Modbus TCP server that blocks the current thread until a shutdown is requested
    pub fn serve_until<S, Sd>(self, service: S, shutdown_signal: Sd)
    where
        S: NewService<Request = Request, Response = Response> + Send + Sync + 'static,
        Sd: Future<Output = ()> + Sync + Send + Unpin + 'static,
        S::Request: From<Request>,
        S::Response: Into<Response>,
        S::Error: Into<Error>,
        S::Instance: Send + Sync + 'static,
    {
        let shutdown_signal = shutdown_signal.fuse();
        let rt = tokio::runtime::Builder::new_multi_thread()
            .enable_io()
            .build()
            .unwrap();

        rt.block_on(async {
            tokio::select! {
                res = self.serve(service) => if let Err(e) = res { error!("error: {}", e) },
                _ = shutdown_signal => trace!("Shutdown signal received")
            }
        })
    }

    pub fn serve_forever<S>(self, service: S)
    where
        S: NewService<Request = Request, Response = Response> + Send + Sync + 'static,
        S::Request: From<Request>,
        S::Response: Into<Response>,
        S::Error: Into<Error>,
        S::Instance: Send + Sync + 'static,
    {
        self.serve_until(service, futures::future::pending())
    }
}

/// The request-response loop spawned by serve_until for each client
async fn process<S>(
    sock_addr: SocketAddr,
    mut framed: Framed<TcpStream, codec::tcp::ServerCodec>,
    service: S,
) -> io::Result<()>
where
    S: Service<Request = Request, Response = Response> + Send + Sync + 'static,
    S::Error: Into<Error>,
{
    loop {
        let request = framed.next().await;
        log::debug!("Received request {:?} from {:?}", request, sock_addr);

        // tcp socket closed
        if request.is_none() {
            log::debug!("Closed connection with {:?}", sock_addr);
            break;
        }

        let request = request.unwrap()?;
        let hdr = request.hdr;
        log::debug!("Calling into service for {:?}", sock_addr);
        let response = service
            .call(request.hdr.unit_id.into(), request.pdu.0)
            .await
            .map_err(Into::into)?;
        
        log::debug!("Finished call into service for {:?}", sock_addr);

        let resp = tcp::ResponseAdu {
            hdr,
            pdu: response.into(),
        };

        log::debug!("Sending Response: {:?} to {:?}", resp, sock_addr);

        framed
            .send(resp)
            .await?;
        log::debug!("Send response frame to {:?}", sock_addr);
    }
    Ok(())
}

/// Start TCP listener - configure and open TCP socket
#[allow(unused)]
fn listener(addr: SocketAddr, workers: usize) -> io::Result<TcpListener> {
    let listener = match addr {
        SocketAddr::V4(_) => Socket::new(Domain::IPV4, Type::STREAM, None)?,
        SocketAddr::V6(_) => Socket::new(Domain::IPV6, Type::STREAM, None)?,
    };
    //configure_tcp(workers, &listener)?;
    listener.reuse_address()?;
    listener.bind(&addr.into())?;
    listener.listen(1024)?;
    listener.set_nonblocking(true)?;
    TcpListener::from_std(listener.into())
}

#[cfg(unix)]
#[allow(unused)]
fn configure_tcp(workers: usize, tcp: &Socket) -> io::Result<()> {
   /*  if workers > 1 {
        tcp.reuse_port()?;
    } */
    Ok(())
}

#[cfg(windows)]
#[allow(unused)]
fn configure_tcp(_workers: usize, _tcp: &Socket) -> io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::Service;
    use crate::slave::Slave;

    use futures::future;

    #[tokio::test]
    async fn service_wrapper() {
        #[derive(Clone)]
        struct DummyService {
            response: Response,
        }

        impl Service for DummyService {
            type Request = Request;
            type Response = Response;
            type Error = Error;
            type Future = future::Ready<Result<Self::Response, Self::Error>>;

            fn call(&self, _: Slave, _: Self::Request) -> Self::Future {
                future::ready(Ok(self.response.clone()))
            }
        }

        let service = DummyService {
            response: Response::ReadInputRegisters(vec![0x33]),
        };

        let slave = Slave(1);
        let pdu = Request::ReadInputRegisters(0, 1);
        let rsp_adu = service.call(slave, pdu).await.unwrap();

        assert_eq!(rsp_adu, service.response);
    }
}
