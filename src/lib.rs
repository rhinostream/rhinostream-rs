extern crate core;

use std::fmt::Debug;
use std::future::Future;
use std::str::FromStr;
use std::time::{Duration, Instant};
use futures::Stream;
use tokio::runtime::Runtime;
use tokio::sync::mpsc::Sender;
use win_desktop_duplication::texture::Texture;
use windows::Win32::Graphics::Direct3D11::{ID3D11Device4, ID3D11DeviceContext4};
use crate::errors::RhinoError;

pub mod stream;
pub mod errors;

pub mod source;
pub mod filter;
pub mod processor;

pub use crate::errors::RhinoResult as Result;

/// A single session of the service. All the internal underlying runtime is controlled by this.
/// if this object is dropped, all of the services associated with this.
pub struct RhinoClient {
    rt: Runtime,
}

pub struct Frame {
    pub data: FrameType,
    pub start_time: Instant,
}

impl Frame {
    fn new(data: FrameType) -> Self {
        Self {
            data,
            start_time: Instant::now(),
        }
    }
    fn new_from(frame: &Frame, data: FrameType) -> Self {
        Self {
            data,
            ..*frame
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

impl FromStr for Resolution {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let x: Vec<_> = s
            .trim()
            .split("x").map(
            |x| {
                return x.parse::<u32>().map_err(|e| format!("parse failed {:?}", e).to_string());
            })
            .collect();
        let x: std::result::Result<Vec<u32>, Self::Err> = x.iter().cloned().collect();

        let x = x?;
        if x.len() != 2 {
            Err("expected WidthxHeight".to_owned())
        } else {
            Ok(Self {
                width: x[0],
                height: x[1],
            })
        }
    }
}

pub enum FrameType {
    Dx11Frame(Texture)
}

#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    pub kind: PacketKind,
    pub start_time: Instant,
    pub encode_time: Duration,
}

impl Packet {
    pub fn new() -> Self {
        Packet {
            data: vec![],
            kind: Default::default(),
            start_time: Instant::now(),
            encode_time: Duration::from_millis(0),
        }
    }
}

#[derive(Clone, Default, Debug)]
pub enum PacketKind {
    #[default]
    Picture,
    Video,
    Audio,
}

#[derive(Default, Clone, Debug)]
pub enum Context {
    #[default]
    None,
    DxContext(DxContext),
}

#[derive(Clone, Debug)]
pub struct DxContext {
    device: ID3D11Device4,
    ctx: ID3D11DeviceContext4,
}

pub trait Config {
    type ConfigType: FromStr<Err=String> + Send;
    fn configure(&mut self, c: Self::ConfigType) -> Result<()>;
    fn configure_from_str(&mut self, c: &str) -> Result<()> {
        let config: Self::ConfigType = c.parse().map_err(|s| RhinoError::ParseError(s))?;
        self.configure(config)
    }
}

pub trait Signal {
    fn signal(&mut self, flags: u32) -> Result<()>;
}

pub trait Source: Signal + Config + Stream<Item=Result<Frame>> + Unpin + Send + 'static {}

pub trait Filter: Config + Unpin + Send + 'static {
    fn apply_filter(&mut self, frame: &Frame) -> Result<Frame>;
}

pub trait Processor: Signal + Config + Unpin + Send + 'static {

    type Future: Future<Output=Result<Packet>> + Send + 'static;

    fn get_queue(&mut self) -> Result<Sender<Frame>>;

    fn get_packet(&mut self, packet: Packet) -> Self::Future;
}