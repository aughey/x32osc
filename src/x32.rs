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

#[derive(Debug,Clone,Copy)]
pub struct ChannelIndex(usize);
#[derive(Debug,Clone,Copy)]
pub struct HeadampIndex(usize);
impl HeadampIndex {
    pub fn new(index: usize) -> Self {
        Self(index)
    }
}

impl ChannelIndex {
    pub fn new(index: usize) -> Self {
        Self(index)
    }
}
impl From<ChannelIndex> for usize {
    fn from(index: ChannelIndex) -> usize {
        index.0
    }
}

impl std::fmt::Display for HeadampIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:0>3}", self.0)
    }
}

impl std::fmt::Display for ChannelIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:0>2}", self.0)
    }
}

/// trait Sender will send a Vec<u8> to a remote location.
pub trait Sender {
    /// Send a Vec<u8> to a remote location.
    fn send(&self, data: Vec<u8>) -> impl Future<Output = anyhow::Result<()>>;
}

/// trait Receiver will receive a Vec<u8> from a remote location.
pub trait Receiver {
    /// Receive a Vec<u8> from a remote location.
    fn receive(&self) -> impl Future<Output = anyhow::Result<Vec<u8>>>;
}

/// The X32 struct provides methods to interact with an X32 mixer.
pub struct X32<S> {
    /// The sender that will send data to the X32.
    sender: S,
    /// An optional receive channel where there is a separate task that is receiving messages.
    receive_channel: Option<Arc<tokio::sync::broadcast::Sender<OscMessage>>>,
}

// methods that are more lower-level core methods of the X32 struct.  This includes send and
// receive methods without any specific command in mind.
impl<S> X32<S>
where
    S: Sender,
{
    /// Creates a new X32 struct with the given sender.
    pub fn new(sender: S) -> Self {
        X32 {
            sender,
            receive_channel: None,
        }
    }

    /// Internal convenience function to recursively messages contained
    /// in a OscPacket to the given broadcast channel.
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

    /// poll_receive will provide a future that will use the provided receiver to receive messages.
    /// The future returned shall be waited for in a separate task to receive messages.
    /// This is necessary because messages from the X32 are sent asynchronously and we need to
    /// be able to receive them at any time.  There is not always a 1 to 1 correspondence between
    /// messages sent and messages received.
    ///
    /// This is a cleaver rust trick to modify the internals of self through the mutable reference,
    /// but since this function returns a future that no longer depends on self, we can start
    /// this receive machinery without holding on to a mutable reference to self or doing any
    /// Mutex or RefCell shenanigans.
    pub fn poll_receive(
        &mut self,
        recv: impl Receiver,
    ) -> impl Future<Output = anyhow::Result<()>> {
        // Create the broadcast channel and update self.
        let (tx, _rx) = tokio::sync::broadcast::channel(16);
        let tx = Arc::new(tx);
        self.receive_channel = Some(tx.clone());

        // We return an async future that only depends on the tx broadcast channel and
        // the receiver that we own.  No dependnencies on self.
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

    /// Internal method to attach to the broadcast receive channel prior to sending a message.
    /// This is called before you send a message so that there isn't a race condition between the
    /// send and the receive where you might miss a message.
    ///
    /// It's important that the signature of this function is not async because
    /// we need to do actual work of subscribing to the broadcast channel first before we
    /// start to wait on it.  If we used async, the future does not run at all until it is awaited.
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

    /// Send and receive a single command to the X32.
    /// A "command", is a single addr message with no arguments that expects a corresponding
    /// reply to be the same as the command.
    async fn send_recv_command(&self, command: &str) -> anyhow::Result<OscMessage> {
        let msg = rosc::OscMessage {
            addr: command.to_string(),
            args: vec![],
        };
        self.send_recv_msg(msg, command).await
    }

    /// Send and receive a single message to the X32.
    async fn send_recv_msg(&self, msg: OscMessage, expect: &str) -> anyhow::Result<OscMessage> {
        let rx = self.start_recv_msg(expect)?;
        self.send_msg(msg).await?;
        rx.await
    }

    /// Send a message to the X32.
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
    /// Get the info from the X32.
    pub async fn info(&self) -> anyhow::Result<Info> {
        let msg = self.send_recv_command("/info").await?;

        Ok(Info {
            osc_version: arg_as_str(&msg, 0)?.to_string(),
            osc_kind: arg_as_str(&msg, 1)?.to_string(),
            name: arg_as_str(&msg, 2)?.to_string(),
            version: arg_as_str(&msg, 3)?.to_string(),
        })
    }

    /// Get the headamp gain from the X32.  The index is 0 based.
    pub async fn headamp_gain(&self, index: HeadampIndex) -> anyhow::Result<f32> {
        let msg = self
            .send_recv_command(&format!("/headamp/{index}/gain"))
            .await?;
        as_float(
            arg(&msg,0)?)
    }

    pub async fn set_headamp_gain(&self, index: HeadampIndex, gain: f32) -> anyhow::Result<()> {
        let msg = OscMessage {
            addr: format!("/headamp/{index}/gain"),
            args: vec![rosc::OscType::Float(gain)],
        };
        self.send_msg(msg).await
    }

    pub async fn channel_index_to_headamp_index(
        &self,
        channel_index: ChannelIndex,
    ) -> anyhow::Result<HeadampIndex> {
        let msg = self
            .send_recv_command(&format!("/-ha/{channel_index}/index"))
            .await?;
        let index = as_i32(
            msg.args
                .get(0)
                .ok_or_else(|| anyhow::anyhow!("Expected arg in position 0"))?,
        )?;
        Ok(HeadampIndex(index.try_into().map_err(|_| {
            anyhow::anyhow!("Invalid headamp index: {index}")
        })?))
    }

    /// Read multiple meters messages from the X32 and average them.
    pub async fn meters_averaged(&self, frames: usize) -> anyhow::Result<[f32; 32]> {
        let mut meters = [0.0; 32];
        for _ in 0..frames {
            let cur_values = self.meters().await?;
            assert_same_type(meters, cur_values);
            for (m, f) in meters.iter_mut().zip(cur_values.iter()) {
                *m += f;
            }
        }
        for m in meters.iter_mut() {
            *m /= frames as f32;
        }
        Ok(meters)
    }

    /// Construct a meters message for the given meter index.
    fn meters_msg(index: u8) -> OscMessage {
        OscMessage {
            addr: "/meters".to_string(),
            args: vec![
                rosc::OscType::String(format!("/meters/{}", index)),
                rosc::OscType::Int(0),
            ],
        }
    }

    /// Read the meters from the X32.  
    pub async fn meters(&self) -> anyhow::Result<[f32; 32]> {
        let reply = self.send_recv_msg(Self::meters_msg(1), "/meters/1").await?;

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

/// Given an OscMessage and an index, return the argument at that index.
/// Returns a Result instead of an Option with additional error information.
fn arg(msg: &OscMessage, index: usize) -> Result<&rosc::OscType> {
    msg.args
        .get(index)
        .ok_or_else(|| anyhow::anyhow!("Expected arg in position {index}"))
}

/// Given an OscType, return a reference to a string or an error.
fn as_str_ref(arg: &rosc::OscType) -> Result<&str> {
    match arg {
        rosc::OscType::String(s) => Ok(s),
        _ => Err(anyhow::anyhow!("Expected string, got {:?}", arg)),
    }
}

/// Give an OscType, return a float or an error.
fn as_float(arg: &rosc::OscType) -> Result<f32> {
    match arg {
        rosc::OscType::Float(f) => Ok(*f),
        _ => Err(anyhow::anyhow!("Expected float, got {:?}", arg)),
    }
}

/// Give an OscType, return an int or an error.
fn as_i32(arg: &rosc::OscType) -> Result<i32> {
    match arg {
        rosc::OscType::Int(i) => Ok(*i),
        _ => Err(anyhow::anyhow!("Expected float, got {:?}", arg)),
    }
}

/// Given an OscMessage and an index, return the argument at that index as a string.
fn arg_as_str(msg: &OscMessage, index: usize) -> Result<&str> {
    let value = arg(msg, index)?;
    as_str_ref(value)
}

/// Take a blob argument and interpret it as a sequence of floats as defined in the x32 icd.
/// This is some great rust magic that will start the parsing and provide an iterator that will on-demand
/// provide the next float without allocating a vector to hold all the floats.
///
/// The format is a u32 little endian value that is the number of floats followed by the floats as le f32.
fn get_float_iter_from_blob<'a>(blob: &'a [u8]) -> Result<impl Iterator<Item = Result<f32>> + 'a> {
    // use nom to get number of float as a 32 bit little endian value
    let (mut rest, mut num_floats) =
        nom::number::complete::le_u32::<_, nom::error::VerboseError<_>>(blob)
            .map_err(|e| anyhow::anyhow!("Failed to get num floats: {e:?}"))?;

    Ok(std::iter::from_fn(move || {
        if num_floats == 0 {
            return None;
        }

        // Try to pull out the next float from the blob
        let (r, float) = match nom::number::complete::le_f32::<_, nom::error::VerboseError<_>>(rest)
        {
            Ok((r, float)) => (r, float),
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
