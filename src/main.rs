use core::num;
use std::{future::Future, net::SocketAddr, sync::Arc};

use anyhow::Result;
use rosc::{OscMessage, OscPacket};
use tokio::net::UdpSocket;

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

    let send_data = {
        let socket = socket.clone();
        |bytes: Vec<u8>| async move {
            socket.send_to(&bytes, remote_addr).await?;
            Ok::<_, anyhow::Error>(())
        }
    };

    let recv_data = {
        let socket = socket.clone();
        || async move {
            let mut buf = vec![0u8; 65536];
            let (len, _) = socket.recv_from(&mut buf).await?;
            // expand here
            let mut v = buf[..len].to_vec();
            v.extend_from_slice(&[0u8; 8]);

            Ok::<_, anyhow::Error>(v)
        }
    };

    let send_recv = {
        let send_data = send_data.clone();
        let recv_data = recv_data.clone();
        |bytes: Vec<u8>| async move {
            send_data(bytes).await?;
            recv_data().await
        }
    };

    // send_osc(
    //     OscMessage {
    //         addr: "/headamp/000/gain".to_string(),
    //         args: vec![rosc::OscType::Float(0.5)],
    //     },
    //     send_data.clone(),
    // )
    // .await?;

    println!(
        "Trim value: {:?}",
        send_recv_command("/headamp/000/gain", send_recv.clone()).await?
    );

    send_recv_command("/info", send_recv.clone()).await?;

    let res = send_recv_osc(
        OscMessage {
            addr: "/meters".to_string(),
            args: vec![
                rosc::OscType::String("/meters/1".to_string()),
                rosc::OscType::Int(0),
            ],
        },
        send_recv.clone(),
    )
    .await?;
    println!("Received packet: {:?}", res);

    let floats = get_floats_from_packet(&res)?;
    println!("Floats: {:?}", floats);

    let recv_until_empty = {
        let socket = socket.clone();
        || async move {
            let mut buf = vec![0u8; 65536];
            let (len, _) = socket.recv_from(&mut buf).await?;
            let buf = buf[..len].to_vec();

            // keep receiving until we cannot receive without blocking.
            let mut second_buf = None;
            loop {
                let mut buf = vec![0u8; 65536];
                match socket.try_recv(buf.as_mut()) {
                    Ok(len) => {
                        let v = buf[..len].to_vec();
                        second_buf = Some(v);
                    }
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(e) => return Err(e),
                }
            }
            let mut buf = if let Some(second_buf) = second_buf {
                second_buf
            } else {
                buf
            };
            // pad
            buf.extend_from_slice(&[0u8; 8]);
            Ok(buf)
        }
    };

    let get_measurement = {
        let send_data = send_data.clone();
        || async move {
            // send the /meters/1 message
            send_osc(
                OscMessage {
                    addr: "/meters".to_string(),
                    args: vec![
                        rosc::OscType::String("/meters/1".to_string()),
                        rosc::OscType::Int(0),
                    ],
                },
                send_data.clone(),
            )
            .await?;

            let buf = recv_until_empty().await?;
            let packet = rosc::decoder::decode_udp(&buf)?;
            let floats = get_floats_from_packet(&packet.1)?;
            let value = floats.get(0).ok_or_else(|| anyhow::anyhow!("No floats"))?;
            Ok::<_, anyhow::Error>(*value)
        }
    };

    let get_value = {
        let send_recv = send_recv.clone();
        || async move {
            let gain = send_recv_command("/headamp/000/gain", send_recv).await?;
            let gain = match gain {
                OscPacket::Message(m) => m,
                _ => anyhow::bail!("Expected message, got {gain:?}"),
            };
            let gain = match gain.args.get(0) {
                Some(rosc::OscType::Float(f)) => f,
                _ => anyhow::bail!("Expected float in first arg of gain message"),
            };
            Ok::<_, anyhow::Error>(*gain)
        }
    };

    let set_value = |value: f32| async move {
        send_osc(
            OscMessage {
                addr: "/headamp/000/gain".to_string(),
                args: vec![rosc::OscType::Float(value)],
            },
            send_data.clone(),
        )
        .await
    };

    let current_gain = get_value().await?;
    println!("Current gain: {:?}", current_gain);

    // Converge on our own here
    // const TARGET: f32 = 0.5;
    // let mut pid = pid::Pid::new(TARGET, 0.1);
    // pid.p(1.0, 1.0);
    // loop {
    //     let cur_value = get_measurement().await?;
    //     let output = pid.next_control_output(cur_value);
    //     println!("Cur value: {:?}, output: {:?}", cur_value, output);
    // }

    return Ok(());

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

async fn send_osc<Fut>(msg: OscMessage, send: impl FnOnce(Vec<u8>) -> Fut) -> Result<()>
where
    Fut: Future<Output = Result<()>>,
{
    let packet = OscPacket::Message(msg);
    let bytes = rosc::encoder::encode(&packet).unwrap();
    send(bytes).await
}

async fn send_recv_osc<FutBuf>(
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

async fn send_recv_command<FutBuf>(
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
    send_recv_osc(msg, send_recv).await
}
