use anyhow::Result;
use rosc::{OscMessage, OscPacket};
use std::{future::Future, net::SocketAddr, sync::Arc};

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
        move |bytes: Vec<u8>| {
            let socket = socket.clone();
            async move {
                socket.send_to(&bytes, remote_addr).await?;
                Ok::<_, anyhow::Error>(())
            }
        }
    };

    let recv_data = {
        let socket = socket.clone();
        move || {
            let socket = socket.clone();
            async move {
                let mut buf = vec![0u8; 65536];
                let (len, _) = socket.recv_from(&mut buf).await?;
                // expand here
                let mut v = buf[..len].to_vec();
                v.extend_from_slice(&[0u8; 8]);

                Ok::<_, anyhow::Error>(v)
            }
        }
    };

    let send_recv = {
        let send_data = send_data.clone();
        let recv_data = recv_data.clone();
        move |bytes: Vec<u8>| {
            let send_data = send_data.clone();
            let recv_data = recv_data.clone();
            async move {
                send_data(bytes).await?;
                recv_data().await
            }
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
        send_recv_command("/headamp/000/gain", &send_recv).await?
    );

    for ch in 0..32 {
        println!(
            "Channel {ch} has headamp source: {:?}",
            send_recv_command(&format!("/-ha/{ch:0>2}/index", ch = ch), &send_recv).await?
        );
    }

    send_recv_command("/info", &send_recv).await?;

    let res = send_recv_osc(
        OscMessage {
            addr: "/meters".to_string(),
            args: vec![
                rosc::OscType::String("/meters/1".to_string()),
                rosc::OscType::Int(0),
            ],
        },
        &send_recv,
    )
    .await?;
    println!("Received packet: {:?}", res);

    let floats = get_floats_from_packet(&res)?;
    println!("Floats: {:?}", floats);

    let recv_until_empty = {
        let socket = socket.clone();
        move || {
            let socket = socket.clone();
            async move {
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
        }
    };

    let get_measurements = {
        let send_data = send_data.clone();
        move || {
            let send_data = send_data.clone();
            let recv_until_empty = recv_until_empty.clone();
            async move {
                // send the /meters/1 message
                send_osc(
                    OscMessage {
                        addr: "/meters".to_string(),
                        args: vec![
                            rosc::OscType::String("/meters/1".to_string()),
                            rosc::OscType::Int(0),
                        ],
                    },
                    &send_data,
                )
                .await?;

                // Read 3 times and average
                let mut sum = vec![0.0; 32];
                const N: usize = 3;
                for _ in 0..N {
                    let buf = recv_until_empty().await?;
                    let packet = rosc::decoder::decode_udp(&buf)?;
                    let floats = get_floats_from_packet(&packet.1)?;
                    for (sum, value) in sum.iter_mut().zip(floats.iter()) {
                        *sum += *value;
                    }
                }
                sum.iter_mut().for_each(|f| *f /= N as f32);
                Ok::<_, anyhow::Error>(sum)
            }
        }
    };

    let get_gain = {
        let send_recv = send_recv.clone();
        move |index: usize| {
            let send_recv = send_recv.clone();
            async move {
                let gain =
                    send_recv_command(&format!("/headamp/{index:0>3}/gain"), &send_recv).await?;
                let gain = match gain {
                    OscPacket::Message(m) => m,
                    _ => anyhow::bail!("Expected message, got {gain:?}"),
                };
                let gain = match gain.args.first() {
                    Some(rosc::OscType::Float(f)) => f,
                    _ => anyhow::bail!("Expected float in first arg of gain message"),
                };
                Ok::<_, anyhow::Error>(*gain)
            }
        }
    };

    let set_gain = |index: usize, value: f32| {
        let send_data = send_data.clone();
        println!("Setting gain: {:?}, {:?}", index, value);
        async move {
            send_osc(
                OscMessage {
                    addr: format!("/headamp/{index:0>3}/gain"),
                    args: vec![rosc::OscType::Float(value)],
                },
                &send_data,
            )
            .await
        }
    };

    // Converge on our own here
    const TARGET: f32 = 0.005;
    const CHANNELS: &[usize] = &[0, 16, 18, 24, 25, 26, 27, 28, 29];

    struct Control {
        pid: pid::Pid<f32>,
        index: usize,
        gain_index: usize,
        current_gain: f32,
    }
    let mut controls = Vec::new();
    for i in CHANNELS {
        let gain_index = {
            // Ask x32 what headamp is assigned to the channel
            let packet =
                send_recv_command(&format!("/-ha/{ch:0>2}/index", ch = i), &send_recv).await?;
            // Get the first argument of the message
            let value = first_arg(&packet)?;
            // interpret it as an int and convert it into whatever we need
            as_int(value)?.try_into()?
        };
        controls.push(Control {
            pid: pid::Pid::new(TARGET, 0.1).p(3.0, 1.0).to_owned(),
            index: *i,
            gain_index,
            current_gain: get_gain(gain_index).await?,
        });
    }

    loop {
        let cur_values = get_measurements().await?;

        for c in controls.iter_mut() {
            let cur_value = *cur_values
                .get(c.index)
                .ok_or_else(|| anyhow::anyhow!("No cur value"))?;
            let output = c.pid.next_control_output(cur_value);
            c.current_gain += output.output;
            set_gain(c.gain_index, c.current_gain).await?;

            println!("Cur value: {:?}, output: {:?}", cur_value, output);
        }
    }

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

    // loop {
    //     let mut buf = vec![0u8; 65536];
    //     let (len, _addr) = socket.recv_from(&mut buf).await?;
    //     let mut buf = buf[..len].to_vec();
    //     buf.extend_from_slice([0u8; 8].as_ref());
    //     let (_, packet) = rosc::decoder::decode_udp(&buf)?;
    //     let floats = get_floats_from_packet(&packet)?;
    //     println!("Received packet: {:?}", floats);
    // }

    #[allow(unreachable_code)]
    Ok(())
}

fn first_arg(packet: &OscPacket) -> Result<&rosc::OscType> {
    let msg = as_msg(packet)?;
    msg.args
        .first()
        .ok_or_else(|| anyhow::anyhow!("Expected at least one arg in message"))
}

fn as_msg(packet: &OscPacket) -> Result<&OscMessage> {
    match packet {
        OscPacket::Message(m) => Ok(m),
        _ => anyhow::bail!("Expected message, got {packet:?}"),
    }
}

fn as_int(arg: &rosc::OscType) -> Result<i32> {
    match arg {
        rosc::OscType::Int(i) => Ok(*i),
        _ => anyhow::bail!("Expected int, got {arg:?}"),
    }
}

fn get_floats_from_packet(packet: &OscPacket) -> Result<Vec<f32>> {
    let meter_message = match packet {
        OscPacket::Message(m) => m,
        _ => anyhow::bail!("Expected message, got {packet:?}"),
    };

    let meter_values = match meter_message.args.first() {
        Some(rosc::OscType::Blob(blob)) => blob,
        _ => anyhow::bail!("Expected blob in first arg of meter message"),
    };
    let meter_values = meter_values.as_slice();

    // use nom to get number of float as a 32 bit little endian value
    let (rest, num_floats) =
        nom::number::complete::le_u32::<_, nom::error::VerboseError<_>>(meter_values)
            .map_err(|e| anyhow::anyhow!("Failed to get num floats: {e:?}"))?;

    let num_floats: usize = num_floats.try_into()?;

    // Parse the floating values
    let (_rest, floats) = nom::multi::count(
        nom::number::complete::le_f32::<_, nom::error::VerboseError<_>>,
        num_floats,
    )(rest)
    .map_err(|e| anyhow::anyhow!("Failed to parse floats: {e:?}"))?;

    Ok(floats)
}

async fn send_osc<Fut>(msg: OscMessage, send: &impl Fn(Vec<u8>) -> Fut) -> Result<()>
where
    Fut: Future<Output = Result<()>>,
{
    let packet = OscPacket::Message(msg);
    let bytes = rosc::encoder::encode(&packet).unwrap();
    send(bytes).await
}

async fn send_recv_osc<FutBuf>(
    msg: OscMessage,
    send_recv: &impl Fn(Vec<u8>) -> FutBuf,
) -> Result<OscPacket>
where
    FutBuf: Future<Output = Result<Vec<u8>>>,
{
    //    println!("Sending message: {:?}", msg);

    let packet = OscPacket::Message(msg);
    let bytes = rosc::encoder::encode(&packet)?;

    //   println!("Sending packet: {bytes:?}");

    let response = send_recv(bytes).await?;

    // receive a packet from the remote address
    let packet = rosc::decoder::decode_udp(&response)
        .map_err(|e| anyhow::anyhow!("Error decoding udp packet: {e:?} {response:?}"))?;
    Ok(packet.1)
}

async fn send_recv_command<FutBuf>(
    command: &str,
    send_recv: &impl Fn(Vec<u8>) -> FutBuf,
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
