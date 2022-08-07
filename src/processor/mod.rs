use std::collections::VecDeque;
use std::future::Future;
use std::mem::swap;
use std::pin::Pin;
use std::str::FromStr;
use std::sync::Arc;
use std::task::Poll;
use futures::Stream;
use log::error;
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use win_desktop_duplication::tex_reader::TextureReader;
use crate::{Config, Context, Frame, Packet, Processor, RhinoError, Result, Signal, FrameType, PacketKind};
use crate::stream::DDA_ERR_MAP;

pub struct CopyTexToCPU {
    tx: Sender<Frame>,
    rx: Arc<Mutex<Receiver<Frame>>>,
    reader: Arc<Mutex<TextureReader>>,
}

impl CopyTexToCPU {
    pub fn new(ctx: &mut Context) -> Result<Self> {
        let (tx, rx) = channel(1);
        let reader = match ctx {
            Context::DxContext(dctx) => {
                Ok(TextureReader::new(dctx.device.clone(), dctx.ctx.clone()))
            }
            _ => {
                Err(RhinoError::UnSupported)
            }
        }?;
        Ok(Self {
            tx,
            rx: Arc::new(Mutex::new(rx)),
            reader: Arc::new(Mutex::new(reader)),
        })
    }

    async fn get_next(rx: Arc<Mutex<Receiver<Frame>>>, reader: Arc<Mutex<TextureReader>>, mut packet: Packet) -> Result<Packet> {
        let mut rx = rx.lock().await;
        let mut reader = reader.lock().await;
        if let Some(frame) = rx.recv().await {
            let tex = match frame.data {
                FrameType::Dx11Frame(tex) => { Ok(tex) }
                _ => { Err(RhinoError::UnSupported) }
            }?;
            reader.get_data(&mut packet.data, &tex).map_err(DDA_ERR_MAP)?;
            packet.start_time = frame.start_time;
            packet.encode_time = frame.start_time.elapsed();
            packet.kind = PacketKind::Picture;
            Ok(packet)
        } else {
            Err(RhinoError::UnSupported)
        }
    }
}


impl Signal for CopyTexToCPU {
    fn signal(&mut self, flags: u32) -> crate::errors::RhinoResult<()> {
        unimplemented!()
    }
}

impl Config for CopyTexToCPU {
    type ConfigType = DummyConfig;

    fn configure(&mut self, c: Self::ConfigType) -> crate::errors::RhinoResult<()> {
        unimplemented!()
    }
}

pub struct DummyConfig {}

impl FromStr for DummyConfig {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(DummyConfig {})
    }
}

impl Unpin for CopyTexToCPU {}

impl Processor for CopyTexToCPU {
    type Future = Pin<Box<dyn Future<Output=Result<(Packet)>> + Send>>;

    fn get_queue(&mut self) -> Sender<Frame> {
        return self.tx.clone();
    }

    fn get_packet(&mut self, packet: Packet) -> Self::Future {
        Box::pin(Self::get_next(self.rx.clone(), self.reader.clone(), packet))
    }
}
