use anyhow::Result;
use rosc::{OscMessage, OscPacket};
use std::{future::Future, sync::Arc};

#[derive(Debug)]
pub struct Info {
    pub osc_version: String,
    pub osc_kind: String,
    pub name: String,
    pub version: String,
}

pub trait Sender {
    fn send(&self, data: Vec<u8>) -> impl Future<Output = anyhow::Result<()>>;
}
pub trait Receiver {
    fn receive(&self) -> impl Future<Output = anyhow::Result<Vec<u8>>>;
}

pub struct X32<S> {
    sender: S,
    receive_channel: Option<Arc<tokio::sync::broadcast::Sender<OscMessage>>>,
}

impl<S> X32<S>
where
    S: Sender,
{
    pub fn new(sender: S) -> Self {
        X32 {
            sender,
            receive_channel: None,
        }
    }

    fn recurse_send(
        tx: &tokio::sync::broadcast::Sender<OscMessage>,
        packet: OscPacket,
    ) -> anyhow::Result<()> {
        match packet {
            OscPacket::Message(msg) => {
                tx.send(msg).map_err(|e| anyhow::anyhow!("{e:?}"))?;
                ()
            }
            OscPacket::Bundle(bundle) => {
                for packet in bundle.content {
                    Self::recurse_send(tx, packet)?;
                }
            }
        };
        Ok(())
    }

    // This method must be called in a task to receive messages.
    // The future returned shall be waited for in a separate task to receive messages.
    pub fn poll_receive(
        &mut self,
        recv: impl Receiver,
    ) -> impl Future<Output = anyhow::Result<()>> {
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let tx = Arc::new(tx);
        self.receive_channel = Some(tx.clone());

        async move {
            loop {
                let mut bytes = recv.receive().await?;
                // pad bytes with 8 zeros
                bytes.extend_from_slice(&[0; 8]);

                let (_, packet) = rosc::decoder::decode_udp(&bytes)
                    .map_err(|e| anyhow::anyhow!("Error decoding udp packet: {e:?} {bytes:?}"))?;

                Self::recurse_send(tx.as_ref(), packet)?;
            }
        }
    }

    // Called before you send a message so that you can start receiving messages before it's actually sent.
    fn start_recv_msg<'a>(
        &self,
        expect: &'a str,
    ) -> Result<impl Future<Output = anyhow::Result<OscMessage>> + 'a> {
        // Subscribe to our receive channel of messages
        let mut rx = self
            .receive_channel
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Receive channel not initialized"))?
            .subscribe();

        Ok(async move {
            // Loop until we receive the expected message
            loop {
                // Get a message from the channel
                let msg = rx
                    .recv()
                    .await
                    .map_err(|e| anyhow::anyhow!("Error receiving message: {e:?}"))?;
                // Return if the message address matches the expected address
                if msg.addr == expect {
                    return Ok(msg);
                }
            }
        })
    }

    async fn send_recv_command(&self, command: &str) -> anyhow::Result<OscMessage> {
        let msg = rosc::OscMessage {
            addr: command.to_string(),
            args: vec![],
        };
        self.send_recv_msg(msg, command).await
    }

    async fn send_recv_msg(&self, msg: OscMessage, expect: &str) -> anyhow::Result<OscMessage> {
        let rx = self.start_recv_msg(expect)?;
        self.send_msg(msg).await?;
        rx.await
    }

    async fn send_msg(&self, msg: OscMessage) -> anyhow::Result<()> {
        let packet = OscPacket::Message(msg);
        let bytes = rosc::encoder::encode(&packet)?;
        self.sender.send(bytes).await
    }
}

impl<S> X32<S>
where
    S: Sender,
{
    pub async fn info(&self) -> anyhow::Result<Info> {
        let msg = self.send_recv_command("/info").await?;

        Ok(Info {
            osc_version: arg_as_string(&msg, 0)?.to_string(),
            osc_kind: arg_as_string(&msg, 1)?.to_string(),
            name: arg_as_string(&msg, 2)?.to_string(),
            version: arg_as_string(&msg, 3)?.to_string(),
        })
    }

    pub async fn meters_averaged(&self, frames: usize) -> anyhow::Result<[f32; 32]> {
        let mut meters = [0.0; 32];
        for _ in 0..frames {
            let frame = self.meters().await?;
            assert_same_type(meters, frame);
            for (m, f) in meters.iter_mut().zip(frame.iter()) {
                *m += f;
            }
        }
        for m in meters.iter_mut() {
            *m /= frames as f32;
        }
        Ok(meters)
    }

    pub async fn meters(&self) -> anyhow::Result<[f32; 32]> {
        let reply = self
            .send_recv_msg(
                OscMessage {
                    addr: "/meters".to_string(),
                    args: vec![
                        rosc::OscType::String("/meters/1".to_string()),
                        rosc::OscType::Int(0),
                    ],
                },
                "/meters/1",
            )
            .await?;

        // The first argument is a blob of floats
        let values = match reply.args.first() {
            Some(rosc::OscType::Blob(blob)) => blob,
            _ => anyhow::bail!("Expected blob in first argument of /meters reply"),
        };

        // Really great rust magic here.  get_float_iter_from_blog will give me an
        // iterator that will on-demand return a Result<f32> for each float in the blob.
        let values = get_float_iter_from_blob(&values)?;

        // Collect the values into an array
        let mut ret = [0.0; 32];
        collect_array(values, &mut ret)?;

        Ok(ret)
    }
}

/// Collects the values from an iterator into a mutable slice.
/// The iterator must be at least as long as the slice.
/// Returns an error if any of the values could not be collected.
/// Returns an error if it could not get enough values from the iterator.
fn collect_array<T>(mut values: impl Iterator<Item = Result<T>>, array: &mut [T]) -> Result<()> {
    for (i, array_value) in array.iter_mut().enumerate() {
        let value = values
            .next()
            .ok_or_else(|| anyhow::anyhow!("Could not get index {} from values", i))??;
        *array_value = value;
    }
    Ok(())
}

// Some convenience functions to make extracting data more convenient for error reporting
fn arg(msg: &OscMessage, index: usize) -> Result<&rosc::OscType> {
    msg.args
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("Expected arg in position {index}"))
}

fn as_string(arg: &rosc::OscType) -> Result<&str> {
    match arg {
        rosc::OscType::String(s) => Ok(s),
        _ => Err(anyhow::anyhow!("Expected string, got {:?}", arg)),
    }
}

fn arg_as_string(msg: &OscMessage, index: usize) -> Result<&str> {
    let value = arg(msg, index)?;
    as_string(value)
}

fn get_floats_from_arg(arg: &rosc::OscType) -> Result<Vec<f32>> {
    let meter_values = match arg {
        rosc::OscType::Blob(blob) => blob,
        _ => anyhow::bail!("Expected blob"),
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

// Take an argument that should be a blob and interpret it as a sequence of floats as defined in the x32 icd.
// This is some great rust magic that will start the parsing and provide an iterator that will on-demand
// provide the next float without allocating a vector to hold all the floats.
fn get_float_iter_from_blob<'a>(blob: &'a [u8]) -> Result<impl Iterator<Item = Result<f32>> + 'a> {
    let meter_values = blob;

    // use nom to get number of float as a 32 bit little endian value
    let (mut rest, num_floats) =
        nom::number::complete::le_u32::<_, nom::error::VerboseError<_>>(meter_values)
            .map_err(|e| anyhow::anyhow!("Failed to get num floats: {e:?}"))?;

    let mut num_floats: usize = num_floats.try_into()?;

    Ok(std::iter::from_fn(move || {
        if num_floats == 0 {
            return None;
        }

        // Try to pull out the next float from the blob
        let (r, float) = match nom::number::complete::le_f32::<_, nom::error::VerboseError<_>>(rest)
        {
            Ok((rest, float)) => (rest, float),
            Err(e) => return Some(Err(anyhow::anyhow!("Failed to parse float: {e:?}"))),
        };

        // Move our indices
        num_floats -= 1;
        rest = r;

        Some(Ok(float))
    }))
}

/// Compile time check that two values are of the same type.
fn assert_same_type<T>(_: T, _: T) {
    // This function's body can be empty.
    // The compiler's type checking enforces that both parameters are of the same type.
}
