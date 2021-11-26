/*!
 * Modbus RTU server functionality
 */

use std::{io::Error, path::Path};
use futures::{select, Future, FutureExt};

use futures_util::{SinkExt, StreamExt};
use tokio_serial::SerialStream;
use tokio_util::codec::Framed;

use crate::{codec, frame::*, prelude::Slave, server::service::{NewService, Service}};

pub struct Server {
    serial: SerialStream,
}

impl Server {
    /// set up a new Server instance from an interface path and baud rate
    pub fn new_from_path<P: AsRef<Path>>(p: P, baud_rate: u32) -> Result<Self, Error> {
        let serial =
            SerialStream::open(&tokio_serial::new(p.as_ref().to_string_lossy(), baud_rate))?;
        Ok(Server { serial })
    }

    /// set up a new Server instance based on a pre-configured SerialStream instance
    pub fn new(serial: SerialStream) -> Self {
        Server { serial }
    }

    /// serve Modbus RTU requests based on the provided service until it finishes
    pub async fn serve_forever<S>(self, new_service: S)
    where
        S: NewService<Request = Request, Response = Response> + Send + Sync + 'static,
        S::Error: Into<Error>,
        S::Instance: 'static + Send + Sync,
    {
        self.serve_until(new_service, futures::future::pending())
            .await;
    }

    /// serve Modbus RTU requests based on the provided service until it finishes or a shutdown signal is received
    pub async fn serve_until<S, Sd>(self, new_service: S, shutdown_signal: Sd)
    where
        S: NewService<Request = Request, Response = Response> + Send + Sync + 'static,
        Sd: Future<Output = ()> + Sync + Send + Unpin + 'static,
        S::Instance: Send + Sync + 'static,
    {
        let framed = Framed::new(self.serial, codec::rtu::ServerCodec::default());
        let service = new_service.new_service().unwrap();
        let future = process(framed, service);

        let mut server = Box::pin(future).fuse();
        let mut shutdown = shutdown_signal.fuse();

        async {
            select! {
                res = server => if let Err(e) = res {
                    println!("error: {}", e);
                },
                _ = shutdown => println!("Shutdown signal received")
            }
        }
        .await;
    }
}

/// frame wrapper around the underlying service's responses to forwarded requests
async fn process<S: Service>(
    mut framed: Framed<SerialStream, codec::rtu::ServerCodec>,
    service: S,
) -> Result<(), Error>
{
    // NOTE this server is running on a single task as the RTU Bus is effectively one long lasting 
    //  connection on which we don't really have to handle parallel requests.
    loop {
        let request = match framed.next().await {
            // Stream is exhausted
            None => break,
            Some(request) => request,
        }?;

        let hdr = request.hdr;
        let response = service
            .call(Slave(request.hdr.slave_id), request.pdu.0.into())
            .await
            .map_err(Into::into)?;

        match response.into() {
            Response::Nop => continue,
            response => framed
            .send(rtu::ResponseAdu {
                hdr,
                pdu: response.into(),
            })
            .await?,
        }
    }
    Ok(())
}
