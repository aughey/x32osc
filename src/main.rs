use anyhow::Result;
use rosc::{OscMessage, OscPacket};
use std::{future::Future, net::SocketAddr, sync::Arc};
use x32osc::x32::{self, ChannelIndex, HeadampIndex};

#[tokio::main]
async fn main() -> Result<()> {
    // async create a udp socket
    let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
    let remote_addr = "10.0.0.50:10023";
    let remote_addr: SocketAddr = remote_addr.parse()?;

    let socket = Arc::new(socket);

    let x32 = x32::X32::new((socket.clone(), remote_addr));

    // Spawn a task to receive messages
    let x32 = {
        let socket = socket.clone();
        let mut x32 = x32;
        let poller = x32.poll_receive(socket);
        tokio::spawn(async move {
            match poller.await {
                Err(e) => eprintln!("Poller error: {:?}", e),
                _ => (),
            }
        });
        x32
    };

    println!("x32 info: {:?}", x32.info().await?);

    for _ in 0..10 {
        println!("Meters: {:?}", x32.meters_averaged(5).await?);
    }

    println!(
        "Gain value for amp 0: {gain}",
        gain=x32.headamp_gain(HeadampIndex::new(0)).await?
    );

    for ch in 0..32 {
        println!(
            "Channel {ch} has headamp source: {headamp:?}",
            headamp = x32.channel_index_to_headamp_index(ChannelIndex::new(ch)).await?
        );
    }

    return Ok(());

    


    // Converge on our own here
    const TARGET: f32 = 0.005;
    const CHANNELS: &[usize] = &[0, 16, 18, 24, 25, 26, 27, 28, 29];
    let channels = CHANNELS.iter().map(|x| ChannelIndex::new(*x));

    struct Control {
        pid: pid::Pid<f32>,
        index: ChannelIndex,
        gain_index: HeadampIndex,
        current_gain: f32,
    }
    let mut controls = Vec::new();
    for i in channels {
        let gain_index = x32.channel_index_to_headamp_index(i).await?;
        controls.push(Control {
            pid: pid::Pid::new(TARGET, 0.1).p(3.0, 1.0).to_owned(),
            index: i,
            gain_index,
            current_gain: x32.headamp_gain(gain_index).await?,
        });
    }

    loop {
        let cur_values = x32.meters_averaged(5).await?;

        for c in controls.iter_mut() {
            let cur_value = *cur_values
                .get(usize::from(c.index))
                .ok_or_else(|| anyhow::anyhow!("No cur value"))?;
            let output = c.pid.next_control_output(cur_value);
            c.current_gain += output.output;
            x32.set_headamp_gain(c.gain_index, c.current_gain).await?;

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
