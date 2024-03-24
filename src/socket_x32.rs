use std::sync::Arc;

use tokio::net::UdpSocket;

use crate::x32::{Receiver, Sender};

impl Sender for (Arc<UdpSocket>, std::net::SocketAddr) {
    fn send(&self, data: Vec<u8>) -> impl std::future::Future<Output = anyhow::Result<()>> {
        let socket = self.0.clone();
        async move {
            // Call send on the UdpSocket explicitly
            UdpSocket::send_to(&socket, &data, self.1).await?;

            Ok(())
        }
    }
}

impl Receiver for Arc<UdpSocket> {
    fn receive(&self) -> impl std::future::Future<Output = anyhow::Result<Vec<u8>>> {
        let socket = self.clone();
        async move {
            let mut buffer = vec![0; 65536];
            let (size, _) = UdpSocket::recv_from(&socket, &mut buffer).await?;

            buffer.resize(size, 0);

            Ok(buffer)
        }
    }
}
