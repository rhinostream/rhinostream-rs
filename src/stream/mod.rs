#[cfg(test)]
mod test {
    use std::sync::mpsc::channel;
    use std::sync::Once;
    use std::thread;
    use log::LevelFilter::{Trace};
    use tokio::runtime::{Builder, Runtime};
    use crate::{Context, Packet};
    use crate::filter::new_nv12_filter;
    use crate::processor::CopyTexToCPU;
    use crate::source::ScreenCap;
    use crate::stream::RhinoStream;

    static ONCE: Once = Once::new();

    fn initialize() {
        ONCE.call_once(|| {
            let _ = env_logger::builder().is_test(true).filter_level(Trace).try_init();
        })
    }

    #[test]
    fn test_stream() {
        initialize();
        let mut ctx = Context::None;
        let src = ScreenCap::new("--screen 0".parse().unwrap(), &mut ctx).unwrap();
        let filter = new_nv12_filter("-r 1920x1080".parse().unwrap(), &mut ctx).unwrap();
        let processor = CopyTexToCPU::new(&mut ctx).unwrap();

        let mut stream = RhinoStream::new( src, filter, processor).unwrap();
        let mut packet = Packet::new();
        for i in 0..1000 {
            stream.get_next_frame(&mut packet).unwrap();
            println!("encode latency: {}ms, size: {}bytes", packet.encode_time.as_millis(), packet.data.len())
        }
    }
}

use std::collections::VecDeque;
use std::mem::swap;
use std::sync::Arc;
use dxfilter::error::DxFilterErr;
use futures::StreamExt;
use log::{error, info, trace, warn};
use tokio::runtime::{Builder, Runtime};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use win_desktop_duplication::errors::DDApiError;
use crate::{Filter, Processor, RhinoError, Source, Result, Packet, Config};

pub(crate) const DDA_ERR_MAP: fn(e: DDApiError) -> RhinoError = |e| {
    match e {
        DDApiError::AccessLost | DDApiError::AccessDenied => {
            RhinoError::Recoverable(format!("{:?}", e))
        }
        _ => {
            RhinoError::UnRecoverable(format!("{:?}", e))
        }
    }
};

pub(crate) const DXF_ERR_MAP: fn(e: DxFilterErr) -> RhinoError = |e| {
    RhinoError::UnRecoverable(format!("{:?}", e))
};


pub(crate) const WIN_ERR_MAP: fn(e: windows::core::Error) -> RhinoError = |e| {
    RhinoError::UnRecoverable(format!("{:?}", e))
};

pub struct RhinoStream<T: Source, U: Filter, V: Processor> {
    rt: Runtime,
    put_back: Sender<Packet>,
    packet_rx: Receiver<Result<Packet>>,

    source_config_tx: Sender<T::ConfigType>,
    filter_config_tx: Sender<U::ConfigType>,
    processor_config_tx: Sender<V::ConfigType>,
}

impl<T: Source, U: Filter, V: Processor> RhinoStream<T, U, V> {
    pub fn new(mut source: T, mut filter: U, processor: V) -> Result<Self> {
        let rt = Builder::new_multi_thread().worker_threads(1).thread_name("Graphics").build().unwrap();

        let (sc_tx, sc_rx) = channel(2);
        let (fc_tx, fc_rx) = channel(2);
        let (pc_tx, pc_rx) = channel(2);
        let (put_back, packet_rx) = Self::start_stream(
            &rt, source, filter, processor, sc_rx, fc_rx, pc_rx,
        )?;
        Ok(Self {
            rt,
            put_back,
            packet_rx,

            source_config_tx: sc_tx,
            filter_config_tx: fc_tx,
            processor_config_tx: pc_tx,
        })
    }
    fn start_stream(
        rt: &Runtime,
        mut source: T,
        mut filter: U,
        mut processor: V,
        sc_rx: Receiver<T::ConfigType>,
        fc_rx: Receiver<U::ConfigType>,
        pc_rx: Receiver<V::ConfigType>,
    ) -> Result<(Sender<Packet>, Receiver<Result<Packet>>)> {
        let processor_queue = processor.get_queue()?;
        let pool = Arc::new(Mutex::new(VecDeque::new()));
        trace!("stream start!");
        rt. spawn(async move {
            let mut src = source;
            let mut filter = filter;
            let mut processor_queue = processor_queue;
            let mut sc_rx = sc_rx;
            let mut fc_rx = fc_rx;
            while let Some(result) = src.next().await {
                trace!("new source!");
                if result.is_err() {
                    let e = result.err().unwrap();
                    match e {
                        RhinoError::Recoverable(s) => {
                            warn!("Stream source recoverable error ignored. `{}`",s);
                            continue;
                        }
                        _ => {
                            error!("Stream source threw unrecoverable error. {:?}",e);
                            break;
                        }
                    }
                }
                let frame = result.unwrap();
                let result = filter.apply_filter(&frame);
                if result.is_err() {
                    let e = result.err().unwrap();
                    match e {
                        RhinoError::Recoverable(s) => {
                            warn!("Filter failed with recoverable error ignored. `{}`",s);
                            continue;
                        }
                        _ => {
                            error!("Filter failed. {:?}",e);
                            break;
                        }
                    }
                }
                let frame = result.unwrap();
                if let Err(_) = processor_queue.send(frame).await {
                    info!("exiting stream loop because processor queue quit");
                    break;
                };

                Self::try_configure(&mut src, &mut sc_rx);
                Self::try_configure(&mut filter, &mut fc_rx);
            }
            info!("exiting stream loop!")
        });

        let pool_1 = pool.clone();
        let (packet_tx, packet_rx) = channel(3);
        rt.spawn(async move {
            let pool = pool_1;
            let mut processor = processor;
            let mut pc_rx = pc_rx;
            loop {
                let mut packet = {
                    let mut locked_pool = pool.lock().await;
                    locked_pool.pop_front().unwrap_or_else(|| Packet::new())
                };
                let fut = processor.get_packet(packet);
                let result = fut.await;
                trace!("sent a packet");
                if packet_tx.send(result).await.is_err() {
                    break;
                };
                trace!("sent a packet");
                Self::try_configure(&mut processor, &mut pc_rx);
            }
        });

        let (pb_tx, pb_rx) = channel(1);
        let pool_2 = pool.clone();
        rt.spawn(
            async move {
                let pool = pool_2;
                let mut pb_rx = pb_rx;
                while let Some(data) = pb_rx.recv().await {
                    {
                        let mut locked_pool = pool.lock().await;
                        locked_pool.push_front(data);
                    }
                }
            }
        );
        Ok((pb_tx, packet_rx))
    }
    fn try_configure<X:Config>(function: &mut X, conf_chan: &mut Receiver<X::ConfigType>) {
        if let Ok(conf) = conf_chan.try_recv() {
            let err = function.configure(conf);
            if let Err(e) = err {
                error!("failed to configure {:?}",e);
            }
        }
    }
    pub fn get_next_frame(&mut self, packet: &mut Packet) -> Result<()> {
        if let Some(new_packet) = self.packet_rx.blocking_recv() {
            let mut new_packet = new_packet?;
            swap(&mut new_packet, packet);
            let _ = self.put_back.blocking_send(new_packet);
        }
        return Ok(());
    }
}