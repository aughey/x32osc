use anyhow::Result;
use std::{net::SocketAddr, sync::Arc};
use x32osc::x32::{self, ChannelIndex, HeadampIndex};

#[tokio::main]
async fn main() -> Result<()> {
    // async create a udp socket
    let (socket, x32) = {
        let socket = tokio::net::UdpSocket::bind("0.0.0.0:0").await?;
        let remote_addr = "10.0.0.50:10023";
        let remote_addr: SocketAddr = remote_addr.parse()?;
        let socket = Arc::new(socket);
        let x32 = x32::X32::new((socket.clone(), remote_addr));
        (socket, x32)
    };

    // Spawn a task to receive messages
    let (join, x32) = {
        // Change our mutability to call poll_receive
        let mut x32 = x32;
        let poller = x32.poll_receive(socket);

        let join = tokio::spawn(async move {
            match poller.await {
                Err(e) => eprintln!("Poller error: {:?}", e),
                _ => (),
            }
        });
        (join, x32)
    };

    // Print the info
    println!("x32 info: {:?}", x32.info().await?);

    // 10 times, print the current meter value
    for _ in 0..10 {
        println!("Meters: {:?}", x32.meters_averaged(5).await?);
    }

    // Print the gain value for the zeroth headamp
    println!(
        "Gain value for amp 0: {gain}",
        gain = x32.headamp_gain(HeadampIndex::new(0)).await?
    );

    // Query the mapping between channel index and headamp index for each channel
    for ch in 0..32 {
        println!(
            "Channel {ch} has headamp source: {headamp:?}",
            headamp = x32
                .channel_index_to_headamp_index(ChannelIndex::new(ch))
                .await
        );
    }

    join.abort();

    return Ok(());

    // Converge on our own here
    #[cfg(feature = "live")]
    {
        const TARGET: f32 = 0.005;
        const CHANNELS: &[usize] = &[0, 16, 18, 24, 25, 26, 27, 28, 29];

        struct Control {
            pid: pid::Pid<f32>,
            mixer_index: usize,
            gain_index: HeadampIndex,
            current_gain: f32,
        }
        let mut controls = Vec::new();
        for i in CHANNELS {
            let mixer_index = *i;
            let channel = ChannelIndex::new(mixer_index);
            let gain_index = x32.channel_index_to_headamp_index(channel).await?;
            controls.push(Control {
                pid: pid::Pid::new(TARGET, 0.1).p(3.0, 1.0).to_owned(),
                mixer_index,
                gain_index,
                current_gain: x32.headamp_gain(gain_index).await?,
            });
        }

        loop {
            let cur_values = x32.meters_averaged(5).await?;

            for c in controls.iter_mut() {
                let cur_value = *cur_values
                    .get(c.mixer_index)
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
}
