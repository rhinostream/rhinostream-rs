use std::{sync, thread};
use std::ffi::c_void;
use std::future::Future;
use std::mem::{swap, transmute, transmute_copy, zeroed};
use std::pin::Pin;
use std::ptr::{null, null_mut};
use std::slice::from_raw_parts;
use std::str::FromStr;
use std::sync::{Arc, RwLock, Weak};
use std::time::Instant;

use clap::{Parser, ValueEnum};
use log::{debug, error, info, trace};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use tokio::sync::Mutex;
use win_desktop_duplication::texture::{Texture, TextureDesc};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_RENDER_TARGET, D3D11_TEXTURE2D_DESC, ID3D11Device4, ID3D11DeviceContext4};
use windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC;
use windows::Win32::System::Threading::{CreateEventExW, CreateEventW, ResetEvent, WaitForSingleObject};

pub use _types::*;
use nvenc_sys as nvenc;
use nvenc_sys::GUID;

use crate::{Config, Context, Frame, FrameType, Packet, PacketKind, Processor, Resolution, Result, RhinoError, Signal};

#[cfg(test)]
mod test {
    use std::sync::Once;

    use clap::ValueEnum;
    use log::LevelFilter::Debug;

    use crate::{Context, Packet};
    use crate::filter::new_ayuv_filter;
    use crate::processor::nvenc::{AdaptiveQuantization, MultiPass, NvEnc, NvencCodec, NvencColor, NvencConfig, Preset, Profile, TuningInfo};
    use crate::Resolution;
    use crate::source::DxDesktopDuplication;
    use crate::stream::RhinoStream;

    static ONCE: Once = Once::new();

    fn initialize() {
        ONCE.call_once(|| {
            let _ = env_logger::builder().is_test(true).filter_level(Debug).try_init();
        })
    }

    #[test]
    fn test_stream() {
        initialize();
        let mut ctx = Context::None;
        let src = DxDesktopDuplication::new("--screen 0".parse().unwrap(), &mut ctx).unwrap();
        let filter = new_ayuv_filter("-r 1920x1080".parse().unwrap(), &mut ctx).unwrap();
        let config: NvencConfig = "-p p1 --profile auto --multi-pass quarter_res -t \
        ultra_low_latency -r 1920x1080 --codec hevc --color argb -b 10000000 -f 144".parse().unwrap();
        let processor = NvEnc::new(&mut ctx, &config).unwrap();

        let mut stream = RhinoStream::new(src, filter, processor).unwrap();
        let mut packet = Packet::new();
        for i in 0..10000 {
            stream.get_next_frame(&mut packet).unwrap();
            println!("encode latency: {} μs, size: {} bytes, {:?}", packet.encode_time.as_micros(), packet.data.len(), &packet.data[0..5])
        }
    }


    #[test]
    fn test_config() {
        println!("{:?}", TuningInfo::value_variants());
        let config: NvencConfig = "-p p3 --profile auto --multi-pass quarter_res -t \
        ultra_low_latency -r 1920x1080 --codec hevc --aq spacial --color nv12 -b 10000000 -f 144".parse().unwrap();
        assert_eq!(config, NvencConfig {
            preset: Preset::P3,
            profile: Profile::Auto,
            multi_pass: MultiPass::QuarterRes,
            tuning_info: TuningInfo::UltraLowLatency,
            level: None,
            color: NvencColor::NV12,
            codec: NvencCodec::HEVC,
            aq: AdaptiveQuantization::Spacial,
            resolution: Resolution { width: 1920, height: 1080 },
            bitrate: 10_000_000,
            framerate: 144.0,
        })
    }
}

mod _types;

pub struct NvEnc {
    nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>,
    enc: *mut c_void,
    conf: NvencConfig,
    preset_config: nvenc::NV_ENC_PRESET_CONFIG,
    init_params: nvenc::NV_ENC_INITIALIZE_PARAMS,

    frame_rx: Arc<Mutex<Receiver<Frame>>>,
    frame_tx: Option<Sender<Frame>>,

    complete_rx: Arc<Mutex<Receiver<NvMappedResource>>>,
    complete_tx: Option<Sender<NvMappedResource>>,
    exec: Arc<Mutex<NvExecutor>>,
    started_background: bool,

    device: ID3D11Device4,
    ctx: ID3D11DeviceContext4,
}

unsafe impl Sync for NvEnc {}

unsafe impl Send for NvEnc {}

impl NvEnc {
    pub fn new(ctx: &mut Context, conf: &NvencConfig) -> Result<Self> {
        let nv = Arc::new(Self::create_api_instance()?);

        let (dev, dev_ctx) = match ctx {
            Context::DxContext(dctx) => {
                Ok((dctx.device.clone(), dctx.ctx.clone()))
            }
            _ => {
                Err(RhinoError::UnSupported)
            }
        }?;

        let enc = Self::open_nvenc_d3d_session(&nv, dev.clone())?;

        trace!("encoder acquired!");

        let conf = conf.clone();
        trace!("encoder preset acquired!");

        let mut preset_config = unsafe { Self::get_nvenc_preset_conf(&nv, enc, &conf) }?;
        let mut init_params = unsafe { Self::get_init_params(&conf, &mut preset_config) };

        Self::init_encoder(&nv, enc, &mut init_params)?;
        trace!("encoder initialized!");

        let (frame_tx, frame_rx) = channel(1);
        let frame_rx = Arc::new(Mutex::new(frame_rx));

        let (complete_tx, complete_rx) = channel(3);
        let complete_rx = Arc::new(Mutex::new(complete_rx));
        let exec = NvExecutor::new(nv.clone(), enc,
                                   preset_config, conf.clone(),
                                   dev.clone(), dev_ctx.clone(), init_params);
        Ok(Self {
            nv,
            enc,
            conf,
            preset_config,
            init_params,

            frame_rx,
            frame_tx: Some(frame_tx),

            complete_rx,
            complete_tx: Some(complete_tx),

            exec,
            started_background: false,

            device: dev.clone(),
            ctx: dev_ctx.clone(),
        })
    }

    async fn frame_collector(rx: Arc<Mutex<Receiver<Frame>>>, tx: Sender<NvMappedResource>, exec: Arc<Mutex<NvExecutor>>) {
        let mut rx = rx.lock().await;
        // Buffer pool initialize to given size

        while let Some(frame) = rx.recv().await {
            trace!("received a frame to process");
            let result = {
                let mut mutexec = exec.lock().await;
                trace!("acquired nvExecutor");
                mutexec.push_frame(&frame).await
            };
            trace!("pushed frame to executor");

            if result.is_err() {
                error!("pushing frame to nvenc failed. {:?}", result.err().unwrap());
                break;
            }
            if tx.send(result.unwrap()).await.is_err() {
                break;
            };
        }
        debug!("frame collector loop exiting")
    }

    fn event_waiter(mut rx: Receiver<NvMappedResource>, tx: Sender<NvMappedResource>) {
        while let Some(resource) = rx.blocking_recv() {
            trace!("waiting on encoder for one item");
            let ev = resource.get_event();
            unsafe {
                WaitForSingleObject(ev.event, 5000);
                ev.reset()
            }
            trace!("encoder is ready to give data!");
            if tx.blocking_send(resource).is_err() {
                break;
            };
        }
        debug!("event wait thread exiting");
    }

    async fn collect_output(rx: Arc<Mutex<Receiver<NvMappedResource>>>, mut packet: Packet) -> Result<Packet> {
        let mut rx = rx.lock().await;
        if let Some(mut res) = rx.recv().await {
            let bitstream = res.get_bitstream_buffer_mut();
            {
                trace!("locking bitstream buffer");
                let locked_buf = bitstream.lock()?;
                trace!("locked bitstream buffer");
                packet.data.resize(locked_buf.slice.len(), 0);
                packet.data.clone_from_slice(locked_buf.slice);
                drop(locked_buf);
                trace!("copied data");
            }
            packet.start_time = res.item.start_time;
            packet.encode_time = packet.start_time.elapsed();
            packet.kind = PacketKind::Video;
            drop(res);
            trace!("returned packet");
            Ok(packet)
        } else {
            Err(RhinoError::Exiting)
        }
    }

    fn create_api_instance() -> Result<nvenc::NV_ENCODE_API_FUNCTION_LIST> {
        let mut nv: nvenc::NV_ENCODE_API_FUNCTION_LIST = unsafe { zeroed() };
        nv.version = nvenc::NV_ENCODE_API_FUNCTION_LIST_VER;
        unsafe { into_err(nvenc::NvEncodeAPICreateInstance(&mut nv))?; }
        Ok(nv)
    }

    fn open_nvenc_d3d_session(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, device: ID3D11Device4) -> Result<*mut c_void> {
        // set init params
        let mut params: nvenc::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = unsafe { zeroed() };
        params.version = nvenc::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
        params.apiVersion = nvenc::NVENCAPI_VERSION;
        unsafe { params.device = transmute(device); }
        params.deviceType = nvenc::_NV_ENC_DEVICE_TYPE_NV_ENC_DEVICE_TYPE_DIRECTX;
        let mut enc = null_mut();

        // call the open session function
        let func = nv.nvEncOpenEncodeSessionEx.as_ref().unwrap();
        let status = unsafe { into_err((*func)(&mut params, &mut enc)) };
        if status.is_err() {
            error!("failed to open encode session. NvencStatus: {:?}",status);
            status?;
        }
        return Ok(enc);
    }

    unsafe fn get_profile_guid(color: &NvencColor, codec: &NvencCodec, profile: &Profile) -> GUID {
        if matches!(color, NvencColor::YUV444 | NvencColor::AYUV) {
            return match codec {
                NvencCodec::H264 => {
                    nvenc::NV_ENC_H264_PROFILE_HIGH_444_GUID
                }
                NvencCodec::HEVC => {
                    nvenc::NV_ENC_HEVC_PROFILE_FREXT_GUID
                }
            };
        }

        match codec {
            NvencCodec::H264 => {
                match profile {
                    Profile::Auto => {
                        nvenc::NV_ENC_CODEC_PROFILE_AUTOSELECT_GUID
                    }
                    Profile::Constrained => {
                        nvenc::NV_ENC_H264_PROFILE_CONSTRAINED_HIGH_GUID
                    }
                    Profile::Baseline => {
                        nvenc::NV_ENC_H264_PROFILE_BASELINE_GUID
                    }
                    Profile::Main => {
                        nvenc::NV_ENC_H264_PROFILE_MAIN_GUID
                    }
                    Profile::High => {
                        nvenc::NV_ENC_H264_PROFILE_HIGH_GUID
                    }
                }
            }
            NvencCodec::HEVC => {
                match profile {
                    Profile::Auto => {
                        nvenc::NV_ENC_CODEC_PROFILE_AUTOSELECT_GUID
                    }
                    _ => {
                        nvenc::NV_ENC_HEVC_PROFILE_MAIN_GUID
                    }
                }
            }
        }
    }

    unsafe fn get_nvenc_preset_conf(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void, nv_conf: &NvencConfig) -> Result<nvenc::NV_ENC_PRESET_CONFIG> {
        let mut preset: nvenc::NV_ENC_PRESET_CONFIG = unsafe { zeroed() };
        preset.version = nvenc::NV_ENC_PRESET_CONFIG_VER;
        preset.presetCfg.version = nvenc::NV_ENC_CONFIG_VER;

        unsafe { preset.presetCfg.profileGUID = Self::get_profile_guid(&nv_conf.color, &nv_conf.codec, &nv_conf.profile); }
        let encode_guid = nv_conf.codec.into();
        let preset_guid = nv_conf.preset.into();
        let tuning_info = nv_conf.tuning_info.into();

        let status = unsafe { (nv.nvEncGetEncodePresetConfigEx.unwrap())(enc, encode_guid, preset_guid, tuning_info, &mut preset) };
        into_err(status)?;

        // set profile once again
        preset.presetCfg.rcParams.set_zeroReorderDelay(1);
        preset.presetCfg.rcParams.averageBitRate = nv_conf.bitrate as _;
        preset.presetCfg.rcParams.vbvBufferSize = Self::get_vbv_buffer_size(120, nv_conf.bitrate as _, nv_conf.framerate as _) as _;
        preset.presetCfg.rcParams.multiPass = nv_conf.multi_pass.into();
        match nv_conf.aq {
            AdaptiveQuantization::Spacial => {
                preset.presetCfg.rcParams.set_enableAQ(1);
            }
            AdaptiveQuantization::Temporal => {
                preset.presetCfg.rcParams.set_enableTemporalAQ(1);
            }
            AdaptiveQuantization::Disabled => {}
        }
        preset.presetCfg.gopLength = nvenc::NVENC_INFINITE_GOPLENGTH;

        match nv_conf.codec {
            NvencCodec::H264 => {
                if let Some(level) = nv_conf.level {
                    preset.presetCfg.encodeCodecConfig.h264Config.level = level;
                }

                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.videoSignalTypePresentFlag = 1;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.videoFormat = 5;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.videoFullRangeFlag = 0;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.colourDescriptionPresentFlag = 1;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.colourPrimaries = 1;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.transferCharacteristics = 1;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.colourMatrix = 1;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.bitstreamRestrictionFlag = 1;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.reserved[0] = 1;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.reserved[1] = 0;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.reserved[2] = 0;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.reserved[3] = 11;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.reserved[4] = 11;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.reserved[5] = 0;
                preset.presetCfg.encodeCodecConfig.h264Config.h264VUIParameters.reserved[6] = 1;


                // settings for multi threading scenarios
                preset.presetCfg.encodeCodecConfig.h264Config.set_enableFillerDataInsertion(0);
                preset.presetCfg.encodeCodecConfig.h264Config.set_outputBufferingPeriodSEI(0);
                preset.presetCfg.encodeCodecConfig.h264Config.set_outputPictureTimingSEI(0);
                preset.presetCfg.encodeCodecConfig.h264Config.set_outputAUD(0);


                preset.presetCfg.encodeCodecConfig.h264Config.set_outputFramePackingSEI(0);
                preset.presetCfg.encodeCodecConfig.h264Config.set_outputRecoveryPointSEI(0);
                preset.presetCfg.encodeCodecConfig.h264Config.set_enableScalabilityInfoSEI(0);
                preset.presetCfg.encodeCodecConfig.h264Config.set_disableSVCPrefixNalu(1);


                preset.presetCfg.encodeCodecConfig.h264Config.set_enableIntraRefresh(1);
                preset.presetCfg.encodeCodecConfig.h264Config.intraRefreshCnt = 10;
                preset.presetCfg.encodeCodecConfig.h264Config.intraRefreshPeriod = 500;
            }
            NvencCodec::HEVC => {
                if let Some(level) = nv_conf.level {
                    preset.presetCfg.encodeCodecConfig.hevcConfig.level = level;
                }

                preset.presetCfg.encodeCodecConfig.hevcConfig.set_enableAlphaLayerEncoding(0);

                preset.presetCfg.encodeCodecConfig.hevcConfig.hevcVUIParameters.videoSignalTypePresentFlag = 1;
                preset.presetCfg.encodeCodecConfig.hevcConfig.hevcVUIParameters.videoFormat = 5;
                preset.presetCfg.encodeCodecConfig.hevcConfig.hevcVUIParameters.videoFullRangeFlag = 0;
                preset.presetCfg.encodeCodecConfig.hevcConfig.hevcVUIParameters.colourDescriptionPresentFlag = 1;
                preset.presetCfg.encodeCodecConfig.hevcConfig.hevcVUIParameters.colourPrimaries = 1;
                preset.presetCfg.encodeCodecConfig.hevcConfig.hevcVUIParameters.transferCharacteristics = 1;
                preset.presetCfg.encodeCodecConfig.hevcConfig.hevcVUIParameters.colourMatrix = 1;

                preset.presetCfg.encodeCodecConfig.hevcConfig.set_enableFillerDataInsertion(0);
                preset.presetCfg.encodeCodecConfig.hevcConfig.set_outputBufferingPeriodSEI(0);
                preset.presetCfg.encodeCodecConfig.hevcConfig.set_outputPictureTimingSEI(0);
                preset.presetCfg.encodeCodecConfig.hevcConfig.set_outputAUD(0);

                preset.presetCfg.encodeCodecConfig.hevcConfig.set_enableAlphaLayerEncoding(0);


                preset.presetCfg.encodeCodecConfig.hevcConfig.set_enableIntraRefresh(1);
                preset.presetCfg.encodeCodecConfig.hevcConfig.intraRefreshCnt = 10;
                preset.presetCfg.encodeCodecConfig.hevcConfig.intraRefreshPeriod = 500;
            }
        }

        if matches!(nv_conf.color,  NvencColor::YUV444) {
            preset.presetCfg.encodeCodecConfig.hevcConfig.set_chromaFormatIDC(3);
            preset.presetCfg.encodeCodecConfig.h264Config.chromaFormatIDC = 3;
        } else {
            preset.presetCfg.encodeCodecConfig.hevcConfig.set_chromaFormatIDC(1);
            preset.presetCfg.encodeCodecConfig.h264Config.chromaFormatIDC = 1;
        }

        Ok(preset)
    }

    unsafe fn get_init_params(nv_conf: &NvencConfig, preset: &mut nvenc::NV_ENC_PRESET_CONFIG) -> nvenc::NV_ENC_INITIALIZE_PARAMS {
        let mut init_params: nvenc::NV_ENC_INITIALIZE_PARAMS = zeroed();
        init_params.version = nvenc::NV_ENC_INITIALIZE_PARAMS_VER;
        init_params.presetGUID = nv_conf.preset.into();
        init_params.encodeGUID = nv_conf.codec.into();
        init_params.tuningInfo = nv_conf.tuning_info.into();

        // set encoding parameters
        init_params.encodeWidth = nv_conf.resolution.width as _;
        init_params.encodeHeight = nv_conf.resolution.height as _;
        init_params.maxEncodeWidth = 3840;
        init_params.maxEncodeHeight = 3840;

        init_params.frameRateNum = (nv_conf.framerate) as _;
        init_params.frameRateDen = 1;

        init_params.bufferFormat = nv_conf.color.into();

        init_params.darWidth = nv_conf.resolution.width as _;
        init_params.darHeight = nv_conf.resolution.height as _;

        init_params.enableEncodeAsync = 1;
        init_params.enablePTD = 1;
        init_params.set_reportSliceOffsets(1);
        init_params.set_enableSubFrameWrite(0);
        init_params.set_enableOutputInVidmem(0);

        init_params.encodeConfig = &mut preset.presetCfg;

        return init_params;
    }

    fn init_encoder(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void, init_params: &mut nvenc::NV_ENC_INITIALIZE_PARAMS) -> Result<()> {
        unsafe { into_err((nv.nvEncInitializeEncoder.unwrap())(enc, init_params)) }
    }

    fn get_vbv_buffer_size(buffer_percent: u64, bitrate: u64, framerate: f32) -> u64 {
        (bitrate * buffer_percent / (framerate * 100f32) as u64) as u64
    }
}

fn into_err(status: nvenc::NVENCSTATUS) -> Result<()> {
    match status {
        nvenc::_NVENCSTATUS_NV_ENC_SUCCESS => Ok(()),
        _ => Err(RhinoError::Unexpected(format!("Nvenc Status: {}", status)))
    }
}


impl Signal for NvEnc {
    fn signal(&mut self, flags: u32) -> Result<()> {
        let exec = self.exec.clone();
        tokio::task::spawn(async move {
            let mut exec = exec.lock().await;
            exec.signal(flags)
        });
        return Ok(());
    }
}

impl Config for NvEnc {
    type ConfigType = NvencConfig;

    fn configure(&mut self, c: Self::ConfigType) -> Result<()> {
        let exec = self.exec.clone();
        tokio::task::spawn(async move {
            let mut exec = exec.lock().await;
            exec.configure(c)
        });
        Ok(())
    }
}

impl Processor for NvEnc {
    type Future = Pin<Box<dyn Future<Output=Result<Packet>> + Send>>;

    fn get_queue(&mut self) -> Result<Sender<Frame>> {
        let mut tx = None;
        swap(&mut tx, &mut self.frame_tx);
        if tx.is_none() {
            Err(RhinoError::Recoverable("get_queue can only be run once".to_owned()))
        } else {
            Ok(tx.unwrap())
        }
    }

    fn get_packet(&mut self, packet: Packet) -> Self::Future {
        if !self.started_background {
            let exec = self.exec.clone();
            let (wait_tx, wait_rx) = channel(4);

            let mut complete_tx = None;
            swap(&mut complete_tx, &mut self.complete_tx);

            let complete_tx = complete_tx.unwrap();

            trace!("spawning frame collector thread");
            tokio::spawn(Self::frame_collector(self.frame_rx.clone(), wait_tx, exec));
            trace!("spawning event waiter thread");
            thread::spawn(move || {
                Self::event_waiter(wait_rx, complete_tx)
            });
            self.started_background = true;
        }

        Box::pin(Self::collect_output(self.complete_rx.clone(), packet))
    }
}


pub struct NvQueueItem {
    item: Option<NvResEventGroup>,
    pool: Weak<NvencPool>,
    pub start_time: Instant,
}

unsafe impl Send for NvQueueItem {}

unsafe impl Sync for NvQueueItem {}

impl NvQueueItem {
    pub fn new(item: NvResEventGroup, pool: Weak<NvencPool>) -> Self {
        trace!("new nvenc queue item");
        Self {
            item: Some(item),
            pool,
            start_time: Instant::now(),
        }
    }

    fn get_item(&self) -> &NvResEventGroup {
        self.item.as_ref().unwrap()
    }
    fn get_item_mut(&mut self) -> &mut NvResEventGroup {
        self.item.as_mut().unwrap()
    }
    pub fn get_event(&self) -> &NvAsyncEvent {
        self.get_item().get_async_event()
    }
    pub fn get_resource(&self) -> &NvRegisteredResource {
        self.get_item().get_resource()
    }

    pub fn get_bitstream_buffer(&self) -> &NvBitstreamBuffer {
        self.get_item().get_bitstream_buffer()
    }

    pub fn get_bitstream_buffer_mut(&mut self) -> &mut NvBitstreamBuffer {
        self.get_item_mut().get_bitstream_buffer_mut()
    }

    pub fn map_resource(self) -> Result<NvMappedResource> {
        let nv = self.get_item().resource.nv.clone();
        let enc = self.get_item().resource.enc;
        NvMappedResource::new(nv, enc, self)
    }
}

impl Drop for NvQueueItem {
    fn drop(&mut self) {
        trace!("dropping queue item");
        if self.item.is_some() {
            if let Some(pool) = self.pool.upgrade() {
                let mut item = None;
                swap(&mut item, &mut self.item);
                if !matches!(self.item, None) {
                    panic!("what the fruck?")
                }
                let mut orig = NvQueueItem {
                    item,
                    pool: self.pool.clone(),
                    start_time: self.start_time,
                };
                tokio::spawn(async move {
                    let pool = pool;
                    pool.put_back(orig).await
                });
            }
        }
    }
}

pub struct NvencPool {
    enc: *mut c_void,
    nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>,
    device: ID3D11Device4,
    state: Mutex<NvencPoolState>,
}

unsafe impl Send for NvencPool {}

unsafe impl Sync for NvencPool {}

struct NvencPoolState {
    pub available: Vec<NvQueueItem>,
    pub desc: TextureDesc,
}

impl NvencPool {
    fn new(nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>, enc: *mut c_void, device: ID3D11Device4) -> Arc<Self> {
        Arc::new(Self {
            nv,
            enc,
            device,
            state: Mutex::new(NvencPoolState {
                available: vec![],
                desc: TextureDesc {
                    height: 0,
                    width: 0,
                    format: Default::default(),
                },
            }),
        })
    }
    fn new_item(self: &Arc<Self>, desc: &TextureDesc) -> Result<NvQueueItem> {
        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: desc.width,
            Height: desc.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: desc.format.into(),
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: Default::default(),
            BindFlags: D3D11_BIND_RENDER_TARGET.0 as _,
            CPUAccessFlags: Default::default(),
            MiscFlags: Default::default(),
        };
        let mut tex = None;
        unsafe {
            self.device.CreateTexture2D(&tex_desc, None, Some(&mut tex))
                .map_err(|e| RhinoError::Unexpected(format!("failed to create texture. {:?}", e)))?;
        }
        let tex = Texture::new(tex.unwrap().into());

        let res = NvRegisteredResource::new(self.nv.clone(), self.enc, tex)?;
        let ev = NvAsyncEvent::new(self.nv.clone(), self.enc)?;
        let buf = NvBitstreamBuffer::new(self.nv.clone(), self.enc)?;

        Ok(NvQueueItem::new(NvResEventGroup::new(res, ev, buf), Arc::downgrade(self)))
    }

    pub(crate) async fn configure(&self, new_desc: &TextureDesc) {
        let mut state = self.state.lock().await;
        if state.desc == *new_desc {
            return;
        }
        state.desc = *new_desc;
        state.available = Vec::new();
    }

    pub(crate) async fn get_item(self: &Arc<Self>) -> Result<NvQueueItem> {
        let mut state = self.state.lock().await;
        let tex = state.available.pop();
        match tex {
            None => {
                self.new_item(&state.desc)
            }
            Some(item) => {
                Ok(item)
            }
        }
    }
    pub(crate) async fn get_desc(&self) -> TextureDesc {
        let state = self.state.lock().await;
        state.desc
    }

    pub(crate) async fn put_back(&self, mut item: NvQueueItem) {
        let mut state = self.state.lock().await;
        if state.desc != item.get_item().resource.res.desc() {
            item.item = None;
        } else {
            state.available.push(item);
        }
    }
}

pub struct NvRegisteredResource {
    enc: *mut c_void,
    reg_resource: nvenc::NV_ENC_REGISTERED_PTR,
    res: Texture,
    nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>,
}

impl Drop for NvRegisteredResource {
    fn drop(&mut self) {
        let _ = NvRegisteredResource::unregister_resource(self.nv.as_ref(), self.enc, self.reg_resource);
    }
}

impl NvRegisteredResource {
    pub fn new(nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>, enc: *mut c_void, tex: Texture) -> Result<Self> {
        let reg_resource = Self::register_resource(nv.as_ref(), enc, &tex)?;
        Ok(Self {
            enc,
            nv,
            res: tex,
            reg_resource,
        })
    }

    fn register_resource(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void, tex: &Texture) -> Result<nvenc::NV_ENC_REGISTERED_PTR> {
        let desc = tex.desc();
        let mut params: nvenc::NV_ENC_REGISTER_RESOURCE = unsafe { zeroed() };
        params.version = nvenc::NV_ENC_REGISTER_RESOURCE_VER;
        params.resourceType = nvenc::_NV_ENC_INPUT_RESOURCE_TYPE_NV_ENC_INPUT_RESOURCE_TYPE_DIRECTX;
        params.height = desc.height;
        params.width = desc.width;
        params.bufferFormat = NvencColor::from(desc.format).into();
        params.bufferUsage = nvenc::_NV_ENC_BUFFER_USAGE_NV_ENC_INPUT_IMAGE;
        unsafe { params.resourceToRegister = transmute_copy(tex.as_raw_ref()); }

        let status = unsafe { nv.nvEncRegisterResource.unwrap()(enc, &mut params) };
        into_err(status)?;

        Ok(params.registeredResource)
    }

    fn unregister_resource(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void,
                           reg_resource: nvenc::NV_ENC_REGISTERED_PTR) -> Result<()> {
        let status = unsafe { nv.nvEncUnregisterResource.unwrap()(enc, reg_resource) };
        into_err(status)
    }
}

pub struct NvAsyncEvent {
    enc: *mut c_void,
    event: HANDLE,
    nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>,
}

impl Drop for NvAsyncEvent {
    fn drop(&mut self) {
        let _ = NvAsyncEvent::unregister_event(self.nv.as_ref(), self.enc, self.event);
        unsafe { CloseHandle(self.event) };
    }
}

impl NvAsyncEvent {
    pub fn new(nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>, enc: *mut c_void) -> Result<Self> {
        let handle = unsafe { CreateEventW(None, true, false, None) }
            .map_err(|e| RhinoError::Unexpected(format!("{:?}", e).to_owned()))?;

        Self::register_event(nv.as_ref(), enc, handle)?;
        Ok(Self {
            enc,
            nv,
            event: handle,
        })
    }

    pub fn handle(&self) -> HANDLE {
        self.event
    }

    pub fn reset(&self) {
        unsafe { ResetEvent(self.event); }
    }

    fn register_event(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void, event: HANDLE) -> Result<()> {
        let mut ev_params: nvenc::NV_ENC_EVENT_PARAMS = unsafe { zeroed() };
        ev_params.version = nvenc::NV_ENC_EVENT_PARAMS_VER;
        unsafe { ev_params.completionEvent = transmute_copy::<isize, *mut c_void>(&event.0); }
        unsafe { into_err(nv.nvEncRegisterAsyncEvent.unwrap()(enc, &mut ev_params)) }
    }

    fn unregister_event(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void,
                        event: HANDLE) -> Result<()> {
        let mut ev_params: nvenc::NV_ENC_EVENT_PARAMS = unsafe { zeroed() };
        ev_params.version = nvenc::NV_ENC_EVENT_PARAMS_VER;
        unsafe { ev_params.completionEvent = transmute_copy::<isize, *mut c_void>(&event.0); }

        let status = unsafe { nv.nvEncUnregisterAsyncEvent.unwrap()(enc, &mut ev_params) };
        into_err(status)
    }
}

pub struct NvBitstreamBuffer {
    enc: *mut c_void,
    buffer_ptr: nvenc::NV_ENC_OUTPUT_PTR,
    nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>,
}

impl Drop for NvBitstreamBuffer {
    fn drop(&mut self) {
        let _ = NvBitstreamBuffer::destroy_buffer(self.nv.as_ref(), self.enc, self.buffer_ptr);
    }
}

impl NvBitstreamBuffer {
    pub fn new(nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>, enc: *mut c_void) -> Result<Self> {
        let buffer_ptr = Self::create_buffer(nv.as_ref(), enc)?;
        Ok(Self {
            enc,
            nv,
            buffer_ptr,
        })
    }

    pub fn buffer_ptr(&self) -> nvenc::NV_ENC_OUTPUT_PTR {
        self.buffer_ptr
    }

    fn lock(&mut self) -> Result<NvLockedBitstream> {
        NvLockedBitstream::new(self)
    }

    fn create_buffer(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void) -> Result<nvenc::NV_ENC_OUTPUT_PTR> {
        let mut buffer_params: nvenc::NV_ENC_CREATE_BITSTREAM_BUFFER = unsafe { zeroed() };
        buffer_params.version = nvenc::NV_ENC_CREATE_BITSTREAM_BUFFER_VER;
        let status = unsafe { nv.nvEncCreateBitstreamBuffer.unwrap()(enc, &mut buffer_params) };
        into_err(status)?;
        Ok(buffer_params.bitstreamBuffer)
    }

    fn destroy_buffer(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void,
                      buf: nvenc::NV_ENC_OUTPUT_PTR) -> Result<()> {
        let status = unsafe { nv.nvEncDestroyBitstreamBuffer.unwrap()(enc, buf) };
        into_err(status)
    }
}

struct NvLockedBitstream<'a> {
    bitstream: &'a mut NvBitstreamBuffer,
    pub slice: &'a [u8],
}

impl Drop for NvLockedBitstream<'_> {
    fn drop(&mut self) {
        let enc = self.bitstream.enc;
        let _ = unsafe { self.bitstream.nv.nvEncUnlockBitstream.unwrap()(enc, self.bitstream.buffer_ptr()) };
    }
}

impl<'a> NvLockedBitstream<'a> {
    fn new(buf: &'a mut NvBitstreamBuffer) -> Result<Self> {
        let enc = buf.enc;
        let mut lock_params: nvenc::NV_ENC_LOCK_BITSTREAM = unsafe { zeroed() };
        lock_params.version = nvenc::NV_ENC_LOCK_BITSTREAM_VER;
        lock_params.set_doNotWait(0);
        lock_params.outputBitstream = buf.buffer_ptr;
        let status = unsafe { buf.nv.nvEncLockBitstream.unwrap()(enc, &mut lock_params) };
        into_err(status)?;

        let slice = unsafe { from_raw_parts(lock_params.bitstreamBufferPtr as *const u8, lock_params.bitstreamSizeInBytes as _) };
        Ok(Self { bitstream: buf, slice })
    }
}

pub struct NvResEventGroup {
    resource: NvRegisteredResource,
    event: NvAsyncEvent,
    buffer: NvBitstreamBuffer,
}

impl NvResEventGroup {
    fn new(res: NvRegisteredResource, ev: NvAsyncEvent, buf: NvBitstreamBuffer) -> Self {
        Self {
            resource: res,
            event: ev,
            buffer: buf,
        }
    }

    fn get_resource(&self) -> &NvRegisteredResource {
        &self.resource
    }

    fn get_async_event(&self) -> &NvAsyncEvent {
        &self.event
    }

    fn get_bitstream_buffer(&self) -> &NvBitstreamBuffer {
        &self.buffer
    }
    fn get_bitstream_buffer_mut(&mut self) -> &mut NvBitstreamBuffer {
        &mut self.buffer
    }
}

pub struct NvMappedResource {
    nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>,
    enc: *mut c_void,
    item: NvQueueItem,
    mapped_res: nvenc::NV_ENC_INPUT_PTR,
}

impl Drop for NvMappedResource {
    fn drop(&mut self) {
        unsafe { self.nv.nvEncUnmapInputResource.unwrap()(self.enc, self.mapped_res) };
    }
}

impl NvMappedResource {
    pub fn new(nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>, enc: *mut c_void, item: NvQueueItem) -> Result<Self> {
        let mapped_res = Self::map_resource(nv.as_ref(), enc, item.get_resource())?;
        Ok(Self {
            nv,
            enc,
            item,
            mapped_res,
        })
    }

    fn map_resource(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, enc: *mut c_void, res: &NvRegisteredResource) -> Result<nvenc::NV_ENC_INPUT_PTR> {
        let mut map_params: nvenc::NV_ENC_MAP_INPUT_RESOURCE = unsafe { zeroed() };
        map_params.version = nvenc::NV_ENC_MAP_INPUT_RESOURCE_VER;
        map_params.registeredResource = res.reg_resource;
        let status = unsafe { nv.nvEncMapInputResource.unwrap()(enc, &mut map_params) };
        into_err(status)?;
        return Ok(map_params.mappedResource);
    }

    fn get_event(&self) -> &NvAsyncEvent {
        return self.item.get_event();
    }
    fn get_bitstream_buffer(&self) -> &NvBitstreamBuffer {
        return self.item.get_bitstream_buffer();
    }
    fn get_bitstream_buffer_mut(&mut self) -> &mut NvBitstreamBuffer {
        return self.item.get_bitstream_buffer_mut();
    }
}

unsafe impl Send for NvMappedResource {}

unsafe impl Sync for NvMappedResource {}

struct NvExecutor {
    nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>,
    enc: *mut c_void,
    preset: nvenc::NV_ENC_PRESET_CONFIG,
    init_params: nvenc::NV_ENC_INITIALIZE_PARAMS,
    signal: u32,
    pic_params: nvenc::NV_ENC_PIC_PARAMS,
    config: NvencConfig,

    pool: Arc<NvencPool>,
    device: ID3D11Device4,
    ctx: ID3D11DeviceContext4,
}

impl NvExecutor {
    pub fn new(
        nv: Arc<nvenc::NV_ENCODE_API_FUNCTION_LIST>, enc: *mut c_void,
        preset: nvenc::NV_ENC_PRESET_CONFIG, config: NvencConfig,
        device: ID3D11Device4, ctx: ID3D11DeviceContext4, init_params: nvenc::NV_ENC_INITIALIZE_PARAMS,
    ) -> Arc<Mutex<Self>> {
        let mut pic_params: nvenc::_NV_ENC_PIC_PARAMS = unsafe { zeroed() };

        pic_params.version = nvenc::NV_ENC_PIC_PARAMS_VER;
        pic_params.inputWidth = config.resolution.width;
        pic_params.inputHeight = config.resolution.height;
        pic_params.inputPitch = 0;

        pic_params.bufferFmt = config.color.into();
        pic_params.inputDuration = (1000f32 / config.framerate) as u64;
        pic_params.pictureStruct = nvenc::_NV_ENC_PIC_STRUCT_NV_ENC_PIC_STRUCT_FRAME;

        let pool = NvencPool::new(nv.clone(), enc, device.clone());
        Arc::new(Mutex::new(Self {
            nv,
            enc,
            preset,
            signal: 0,
            pic_params,
            config,
            pool,
            device,
            init_params,
            ctx,
        }))
    }

    pub async fn push_frame(&mut self, frame: &Frame) -> Result<NvMappedResource> {
        let tex = match &frame.data {
            FrameType::Dx11Frame(tex) => { tex }
            _ => {
                unimplemented!()
            }
        };

        let tex_desc = tex.desc();
        if tex_desc != self.pool.get_desc().await {
            self.pool.configure(&tex.desc()).await;
            self.config.resolution = Resolution { width: tex_desc.width, height: tex_desc.height };
            self.configure(self.config.clone())?;
        }

        // get new queue item
        let mut item = self.pool.get_item().await?;
        item.start_time = frame.start_time;

        let buffer_tex = &item.get_resource().res;

        // copy texture into buffer
        unsafe { self.ctx.CopyResource(buffer_tex.as_raw_ref(), tex.as_raw_ref()); }
        let mapped = item.map_resource()?;

        let enc_params = self.get_enc_picparams(&mapped);
        let status = unsafe { self.nv.nvEncEncodePicture.unwrap()(self.enc, enc_params) };
        into_err(status)?;

        Ok(mapped)
    }

    pub fn get_enc_picparams(&mut self, item: &NvMappedResource) -> *mut nvenc::NV_ENC_PIC_PARAMS {
        let enc_params = &mut self.pic_params;

        if self.signal != 0 {
            enc_params.encodePicFlags = (
                nvenc::_NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_OUTPUT_SPSPPS |
                    nvenc::_NV_ENC_PIC_FLAGS_NV_ENC_PIC_FLAG_FORCEIDR
            ) as _;
            self.signal = 0;
        } else {
            enc_params.encodePicFlags = 0;
        }
        enc_params.inputBuffer = item.mapped_res;
        enc_params.inputTimeStamp = item.item.start_time.elapsed().as_millis() as _;
        enc_params.outputBitstream = item.get_bitstream_buffer().buffer_ptr();
        enc_params.completionEvent = item.get_event().event.0 as nvenc::HANDLE;

        enc_params
    }

    pub fn configure(&mut self, conf: NvencConfig) -> Result<()> {
        let init_params = &mut self.init_params;
        init_params.encodeHeight = conf.resolution.height as _;
        init_params.encodeWidth = conf.resolution.width as _;
        init_params.frameRateNum = (conf.framerate) as _;
        init_params.frameRateDen = 1;

        self.preset.presetCfg.rcParams.averageBitRate = conf.bitrate as _;
        self.preset.presetCfg.rcParams.vbvBufferSize = NvEnc::get_vbv_buffer_size(120, conf.bitrate as _, conf.framerate as _) as _;

        init_params.encodeConfig = &mut self.preset.presetCfg;


        let mut reinit_params: nvenc::NV_ENC_RECONFIGURE_PARAMS = unsafe { zeroed() };
        reinit_params.version = nvenc::NV_ENC_RECONFIGURE_PARAMS_VER;
        reinit_params.reInitEncodeParams = *init_params;
        reinit_params.set_forceIDR(1);
        let status = unsafe { self.nv.nvEncReconfigureEncoder.unwrap()(self.enc, &mut reinit_params) };
        into_err(status)
    }

    pub fn signal(&mut self, flags: u32) -> Result<()> {
        self.signal = flags;
        Ok(())
    }
}

unsafe impl Send for NvExecutor {}

unsafe impl Sync for NvExecutor {}