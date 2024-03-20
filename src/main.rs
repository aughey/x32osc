use core::num;
use std::{future::Future, net::SocketAddr, sync::Arc};

use anyhow::Result;
use rosc::{OscMessage, OscPacket};

#[tokio::main]
async fn main() -> Result<()> {
    // async create a udp socket
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    let remote_addr = "10.0.0.50:10023";
    let remote_addr: SocketAddr = remote_addr.parse()?;

    // Create an /info osc message
    let msg = OscMessage {
        addr: "/info".to_string(),
        args: vec![],
    };

    let packet = OscPacket::Message(msg);
    let packet_bytes = rosc::encoder::encode(&packet)?;

    println!("Info message: {:?}", packet_bytes);

    // Send the packet to the remote address
    socket.send_to(&packet_bytes, remote_addr).await?;

    // Receive a packet from the remote address
    let mut buf = vec![0u8; 65536];
    let (len, addr) = socket.recv_from(&mut buf).await?;
    let packet = rosc::decoder::decode_udp(&buf[..len])?;
    println!("Received packet: {:?} from {:?}", packet, addr);

    let socket = Arc::new(socket);

    let send_recv = {
        let socket = socket.clone();
        |bytes: Vec<u8>| async move {
            socket.send_to(&bytes, remote_addr).await?;
            let mut buf = vec![0u8; 65536];
            let (len, _) = socket.recv_from(&mut buf).await?;
            println!("Got response: {len} bytes");

            let mut v = buf[..len].to_vec();
            // x32 doesn't pad blob types, so we push 8 nulls at the end of the buffer
            v.extend_from_slice(&[0u8; 8]);
            Ok(v)
        }
    };

    println!(
        "Trim value: {:?}",
        send_command("/headamp/000/gain", send_recv.clone()).await?
    );

    return Ok(());

    send_command("/info", send_recv.clone()).await?;

    let res = send_osc(
        OscMessage {
            addr: "/meters".to_string(),
            args: vec![
                rosc::OscType::String("/meters/1".to_string()),
                rosc::OscType::Int(0),
            ],
        },
        send_recv,
    )
    .await?;
    println!("Received packet: {:?}", res);

    let floats = get_floats_from_packet(&res)?;

    // let inputs = floats
    //     .get(0..32)
    //     .ok_or_else(|| anyhow::anyhow!("Not enough floats for input"))?;
    // let gate_gain = floats
    //     .get(32..32 + 32)
    //     .ok_or_else(|| anyhow::anyhow!("Not enough floats for gain"))?;
    // let dynamics = floats
    //     .get(32 + 32..32 + 32 + 32)
    //     .ok_or_else(|| anyhow::anyhow!("Not enough floats for dynamics"))?;

    // println!("Inputs: {:?}", inputs);
    // println!("Gate gain: {:?}", gate_gain);
    // println!("Dynamics: {:?}", dynamics);

    loop {
        let mut buf = vec![0u8; 65536];
        let (len, _addr) = socket.recv_from(&mut buf).await?;
        let mut buf = buf[..len].to_vec();
        buf.extend_from_slice([0u8; 8].as_ref());
        let (_, packet) = rosc::decoder::decode_udp(&buf)?;
        let floats = get_floats_from_packet(&packet)?;
        println!("Received packet: {:?}", floats);
    }

    #[allow(unreachable_code)]
    Ok(())
}

fn get_floats_from_packet(packet: &OscPacket) -> Result<Vec<f32>> {
    let meter_message = match packet {
        OscPacket::Message(m) => m,
        _ => anyhow::bail!("Expected message, got {packet:?}"),
    };

    let meter_values = match meter_message.args.get(0) {
        Some(rosc::OscType::Blob(blob)) => blob,
        _ => anyhow::bail!("Expected blob in first arg of meter message"),
    };
    let meter_values = meter_values.as_slice();

    // use nom to get number of float as a 32 bit little endian value
    let (rest, num_floats) =
        nom::number::complete::le_u32::<_, nom::error::VerboseError<_>>(meter_values)
            .map_err(|e| anyhow::anyhow!("Failed to get num floats: {e:?}"))?;

    let num_floats: usize = num_floats.try_into()?;
    println!("num_floats: {num_floats}");

    // Parse the floating values
    let (_rest, floats) = nom::multi::count(
        nom::number::complete::le_f32::<_, nom::error::VerboseError<_>>,
        num_floats,
    )(rest)
    .map_err(|e| anyhow::anyhow!("Failed to parse floats: {e:?}"))?;

    println!("Floats: {}, {:?}", floats.len(), floats);

    Ok(floats)
}

async fn send_osc<FutBuf>(
    msg: OscMessage,
    send_recv: impl FnOnce(Vec<u8>) -> FutBuf,
) -> Result<OscPacket>
where
    FutBuf: Future<Output = Result<Vec<u8>>>,
{
    println!("Sending message: {:?}", msg);

    let packet = OscPacket::Message(msg);
    let bytes = rosc::encoder::encode(&packet)?;

    println!("Sending packet: {bytes:?}");

    let response = send_recv(bytes).await?;

    // receive a packet from the remote address
    let packet = rosc::decoder::decode_udp(&response)
        .map_err(|e| anyhow::anyhow!("Error decoding udp packet: {e:?} {response:?}"))?;
    Ok(packet.1)
}

async fn send_command<FutBuf>(
    command: &str,
    send_recv: impl FnOnce(Vec<u8>) -> FutBuf,
) -> Result<OscPacket>
where
    FutBuf: Future<Output = Result<Vec<u8>>>,
{
    let msg = OscMessage {
        addr: command.to_string(),
        args: vec![],
    };
    send_osc(msg, send_recv).await
}
