extern crate env_logger;
use std::ffi::{c_char, CStr};
use std::ptr::{null, null_mut};
use std::str::FromStr;
use std::sync::{Mutex, Once, OnceLock};
use std::time::Duration;

use dxfilter::DxFilter;
use env_logger::init;
use log::{error, trace};

use crate::{Config, Context, Filter, Frame, Packet};
use crate::errors::{RhinoError, RhinoResult};
use crate::filter::{DxColorConfig, DxColorFilterType, new_argb_filter, new_ayuv_filter, new_nv12_filter};
use crate::processor::nvenc::{NvEnc, NvencConfig};
use crate::source::{DxDesktopDuplication, ScreenCapConfig};
use crate::stream::{ConfigType, RhinoStream, SignalType};

#[repr(u8)]
#[derive(Eq, PartialEq, Clone, Copy, Debug)]
pub enum Error {
    Ok,
    InvalidInput,
    InvalidConfig,
    InitFailed,

    NotFound,
    Exiting,

    Recoverable,
    UnRecoverable,

    Unexpected,

    Timeout,
}

impl Config for Box<dyn Filter<ConfigType=DxColorConfig>> {
    type ConfigType = DxColorConfig;

    fn configure(&mut self, c: DxColorConfig) -> RhinoResult<()> {
        (**self).configure(c)
    }
}

impl Filter for Box<dyn Filter<ConfigType=DxColorConfig>> {
    fn apply_filter(&mut self, frame: &Frame) -> RhinoResult<Frame> {
        (**self).apply_filter(frame)
    }
}
type NvencStream = RhinoStream<DxDesktopDuplication, Box<dyn Filter<ConfigType=DxColorConfig>>, NvEnc>;
#[derive(Debug, Clone)]
pub struct StreamBuilder {
    screen_cap_config: ScreenCapConfig,
    filter_config: DxColorConfig,
    nvenc_config: NvencConfig,
}

impl Default for StreamBuilder {
    fn default() -> Self {
        return Self {
            screen_cap_config: ScreenCapConfig::from_str("").unwrap(),
            filter_config: DxColorConfig::from_str("").unwrap(),
            nvenc_config: NvencConfig::from_str("").unwrap(),
        };
    }
}
#[no_mangle]
pub extern "C" fn new_stream_builder() -> *mut StreamBuilder {
    unsafe {
        let builder = Box::new(Default::default());
        Box::into_raw(builder)
    }
}

#[no_mangle]
unsafe extern "C" fn free_builder(builder: *mut StreamBuilder) {
    if !builder.is_null() {
        drop(Box::from_raw(builder));
    }
}

#[no_mangle]
unsafe extern "C" fn with_screen_cap_opt(builder: *mut StreamBuilder, opt: *const c_char) -> Error {
    if builder.is_null() || opt.is_null() {
        return Error::InvalidInput;
    }
    let opt = CStr::from_ptr(opt);
    if let Ok(options) = opt.to_str() {
        if let Ok(conf) = ScreenCapConfig::from_str(options) {
            (*builder).screen_cap_config = conf;
            Error::Ok
        } else {
            Error::InvalidConfig
        }
    } else {
        Error::InvalidInput
    }
}
#[no_mangle]
unsafe extern "C" fn with_filter_opt(builder: *mut StreamBuilder, opt: *const c_char) -> Error {
    if builder.is_null() {
        return Error::InvalidInput;
    }
    let opt = CStr::from_ptr(opt);
    if let Ok(options) = opt.to_str() {
        if let Ok(conf) = DxColorConfig::from_str(options) {
            (*builder).filter_config = conf;
            Error::Ok
        } else {
            Error::InvalidConfig
        }
    } else {
        Error::InvalidInput
    }
}
#[no_mangle]
unsafe extern "C" fn with_nvenc_opt(builder: *mut StreamBuilder, opt: *const c_char) -> Error {
    if builder.is_null() {
        return Error::InvalidInput;
    }
    let opt = CStr::from_ptr(opt);
    if let Ok(options) = opt.to_str() {
        if let Ok(conf) = NvencConfig::from_str(options) {
            (*builder).nvenc_config = conf;
            Error::Ok
        } else {
            Error::InvalidConfig
        }
    } else {
        Error::InvalidInput
    }
}

macro_rules! handle_err {
    ($e:expr, $t:expr) => {
        if $e.is_err() {
            error!("stream err: {:?}",$e.err());
            return $t;
        } else {
            $e.unwrap()
        }
    };
}

#[no_mangle]
unsafe extern "C" fn build_stream(builder: *mut StreamBuilder, nvenc_stream_out: *mut *mut NvencStream) -> Error {
    if nvenc_stream_out.is_null() || builder.is_null() {
        return Error::InvalidInput;
    }

    // consumes the builder and generates the stream object
    let b = Box::from_raw(builder);
    let mut ctx = Context::default();


    let source = DxDesktopDuplication::new(b.screen_cap_config, &mut ctx);
    let source = handle_err!(source, Error::InvalidConfig);
    trace!("acquired desktop duplication");

    let filter: RhinoResult<Box<dyn Filter<ConfigType=DxColorConfig>>> = match b.filter_config.color {
        DxColorFilterType::NV12Filter => { new_nv12_filter(b.filter_config.clone(), &mut ctx).map(|f| Box::new(f) as _) }
        DxColorFilterType::RGBFilter => { new_argb_filter(b.filter_config.clone(), &mut ctx).map(|f| Box::new(f) as _) }
        DxColorFilterType::AYUVFilter => { new_ayuv_filter(b.filter_config.clone(), &mut ctx).map(|f| Box::new(f) as _) }
    };
    let filter = handle_err!( filter, Error::InvalidConfig);
    trace!("acquired filter");

    let enc = NvEnc::new(&mut ctx, &b.nvenc_config);
    let enc = handle_err!(enc, Error::InvalidConfig);
    trace!("acquired processor");

    let stream = RhinoStream::new(source, filter, enc);
    let stream = handle_err!(stream, Error::InitFailed);
    trace!("acquired stream");

    let stream = Box::new(stream);
    let ptr = Box::into_raw(stream);
    *nvenc_stream_out = ptr;
    trace!("returning");
    return Error::Ok;
}

#[cfg(test)]
mod test {
    use std::ffi::CString;
    use std::ptr::null_mut;
    use std::sync::Once;
    use std::time::{Duration, Instant};

    use log::LevelFilter::Trace;

    use crate::clib::{build_stream, Error, free_packet, free_stream, get_next_frame, new_packet, new_stream_builder, NvencStream, with_filter_opt, with_nvenc_opt, with_screen_cap_opt};
    use crate::filter::{DxColorConfig, DxColorFilterType};
    use crate::processor::nvenc::{AdaptiveQuantization, MultiPass, NvencCodec, NvencColor, NvencConfig, Preset, Profile, TuningInfo};
    use crate::Resolution;
    use crate::source::ScreenCapConfig;

    static ONCE: Once = Once::new();

    fn initialize() {
        ONCE.call_once(|| {
            let _ = env_logger::builder().is_test(true).filter_level(Trace).try_init();
        })
    }

    #[test]
    fn test_config_builder() {
        initialize();
        unsafe {
            let new_builder = new_stream_builder();
            assert_eq!(with_screen_cap_opt(new_builder, CString::new("--screen 0 -r 1920x1080 -f 60.0").unwrap().as_ptr()), Error::Ok);

            assert_eq!((*new_builder).screen_cap_config, ScreenCapConfig {
                screen: 1,
                refresh_rate: 60.0,
                resolution: Some(Resolution { width: 1920, height: 1080 }),
                skip_cursor: false,
            });

            assert_eq!(with_filter_opt(new_builder, CString::new("-c nv12 -r 1920x1080").unwrap().as_ptr()), Error::Ok);
            assert_eq!((*new_builder).filter_config, DxColorConfig {
                resolution: Resolution { width: 1920, height: 1080 },
                color: DxColorFilterType::NV12Filter,
            });


            assert_eq!(with_nvenc_opt(new_builder, CString::new("-p p4 --profile auto --multi-pass disabled --aq disabled -t \
        low_latency -r 1920x1080 --codec hevc --color nv12 -b 10000000 -f 60").unwrap().as_ptr()), Error::Ok);
            assert_eq!((*new_builder).nvenc_config, NvencConfig {
                level: None,
                preset: Preset::P4,
                profile: Profile::Auto,
                multi_pass: MultiPass::Disabled,
                tuning_info: TuningInfo::LowLatency,
                color: NvencColor::NV12,
                codec: NvencCodec::HEVC,
                aq: AdaptiveQuantization::Disabled,
                resolution: Resolution { width: 1920, height: 1080 },
                bitrate: 10000000,
                framerate: 60.0,
            });
        }
    }

    #[test]
    fn test_init() {
        initialize();
        let builder = new_stream_builder();
        unsafe {
            with_screen_cap_opt(builder, CString::new("--screen 0").unwrap().as_ptr());
            with_filter_opt(builder, CString::new("-c nv12 -r 1920x1080").unwrap().as_ptr());
            with_nvenc_opt(builder, CString::new("--color nv12 --codec h264 -r 1920x1080 -b 10000000 -f 60.0").unwrap().as_ptr());
            let mut stream: *mut NvencStream = null_mut();
            let error = build_stream(builder, &mut stream);
            assert_eq!(error, Error::Ok);
            let packet = new_packet();
            let i = Instant::now();
            while i.elapsed() < Duration::from_secs(5) {
                get_next_frame(stream, packet, 500);
                println!("encode duration: {} ms, bytes: {:?}", (*packet).encode_time.as_millis(), (*packet).data[0..10].iter())
            }
            free_packet(packet);
            free_stream(stream);
        }
    }
}

#[no_mangle]
unsafe extern "C" fn new_packet() -> *mut Packet {
    let p = Box::new(Packet::new());
    return Box::into_raw(p);
}

#[no_mangle]
unsafe extern "C" fn get_data(packet: *mut Packet, data: *mut *const u8, len: *mut usize) -> Error {
    if len.is_null() || data.is_null() || packet.is_null() {
        return Error::InvalidInput;
    }
    *len = (*packet).data.len();
    *data = (*packet).data.as_ptr();

    Error::Ok
}

#[no_mangle]
unsafe extern "C" fn free_packet(packet: *mut Packet) {
    if !packet.is_null() {
        let packet = Box::from_raw(packet);
        drop(packet)
    }
}

#[no_mangle]
unsafe extern "C" fn free_stream(stream: *mut NvencStream) {
    if stream.is_null() {
        return;
    }
    let b = Box::from_raw(stream);
    drop(b);
}

#[no_mangle]
unsafe extern "C" fn request_idr(raw_stream: *mut NvencStream) {
    let mut stream = Box::from_raw(raw_stream);
    stream.signal(SignalType::Processor(1));
    Box::into_raw(stream);
}

#[no_mangle]
unsafe extern "C" fn get_next_frame(raw_stream: *mut NvencStream, raw_packet: *mut Packet, timeout_ms: u64) -> Error {
    if raw_packet.is_null() || raw_stream.is_null() {
        return Error::InvalidInput;
    }

    let mut stream = Box::from_raw(raw_stream);
    let mut packet = Box::from_raw(raw_packet);


    let result = stream.get_next_frame_with_timeout(&mut packet, Duration::from_millis(timeout_ms));


    Box::into_raw(stream);
    Box::into_raw(packet);

    match result {
        Ok(_) => {
            Error::Ok
        }
        Err(e) => {
            match e {
                RhinoError::UnSupported => {
                    Error::InvalidConfig
                }
                RhinoError::NotFound => {
                    Error::NotFound
                }
                RhinoError::Exiting => {
                    Error::Exiting
                }
                RhinoError::Recoverable(_) => {
                    Error::Recoverable
                }
                RhinoError::UnRecoverable(_) => {
                    Error::UnRecoverable
                }
                RhinoError::ParseError(_) => {
                    Error::InvalidInput
                }
                RhinoError::Unexpected(_) => {
                    Error::UnRecoverable
                }
                RhinoError::Timedout => {
                    Error::Timeout
                }
            }
        }
    }
}

#[no_mangle]
unsafe extern "C" fn configure_source(raw_stream: *mut NvencStream, opt: *const c_char) -> Error {
    let mut stream = Box::from_raw(raw_stream);
    let opt = CStr::from_ptr(opt);
    let result = if let Ok(opt_str) = opt.to_str() {
        if let Ok(config) = ScreenCapConfig::from_str(opt_str) {
            let res = stream.configure(ConfigType::Source(config));
            if res.is_err() {
                error!("Failed to configure the stream, err: {:?}", res);
                Error::InvalidInput
            } else {
                Error::Ok
            }
        } else {
            Error::InvalidInput
        }
    } else {
        Error::InvalidInput
    };
    Box::into_raw(stream);
    result
}

#[no_mangle]
unsafe extern "C" fn configure_filter(raw_stream: *mut NvencStream, opt: *const c_char) -> Error {
    let mut stream = Box::from_raw(raw_stream);
    let opt = CStr::from_ptr(opt);
    let result = if let Ok(opt_str) = opt.to_str() {
        if let Ok(config) = DxColorConfig::from_str(opt_str) {
            let res = stream.configure(ConfigType::Filter(config));
            if res.is_err() {
                error!("Failed to configure the stream, err: {:?}", res);
                Error::InvalidInput
            } else {
                Error::Ok
            }
        } else {
            Error::InvalidInput
        }
    } else {
        Error::InvalidInput
    };
    Box::into_raw(stream);
    result
}

#[no_mangle]
unsafe extern "C" fn configure_processor(raw_stream: *mut NvencStream, opt: *const c_char) -> Error {
    let mut stream = Box::from_raw(raw_stream);
    let opt = CStr::from_ptr(opt);
    let result = if let Ok(opt_str) = opt.to_str() {
        if let Ok(config) = NvencConfig::from_str(opt_str) {
            let res = stream.configure(ConfigType::Processor(config));
            if res.is_err() {
                error!("Failed to configure the stream, err: {:?}", res);
                Error::InvalidInput
            } else {
                Error::Ok
            }
        } else {
            Error::InvalidInput
        }
    } else {
        Error::InvalidInput
    };
    Box::into_raw(stream);
    result
}

static ONCE: Once = Once::new();
#[no_mangle]
extern "C" fn init_logger() {
    ONCE.call_once(|| {
        let _ = init();
    })
}