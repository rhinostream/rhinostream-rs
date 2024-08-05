use std::ffi::c_void;
use std::mem::swap;
use std::pin::Pin;
use std::str::FromStr;
use std::task::Poll;

use clap::{Parser,ArgAction};
use futures::{Future, FutureExt, select, Stream};
use log::{info, warn};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use win_desktop_duplication::{co_init, DesktopDuplicationApi, DuplicationApiOptions, set_process_dpi_awareness};
use win_desktop_duplication::devices::{Adapter, AdapterFactory};
use win_desktop_duplication::errors::DDApiError;
use win_desktop_duplication::outputs::{Display, DisplayMode};
use windows::core::Interface;
use windows::Win32::Graphics::Dxgi::IDXGIDevice4;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentThread, HIGH_PRIORITY_CLASS, REALTIME_PRIORITY_CLASS, SetPriorityClass, SetThreadPriority, THREAD_PRIORITY_HIGHEST, THREAD_PRIORITY_TIME_CRITICAL};

use crate::{Config, Context, DxContext, Frame, FrameType, Resolution, Result, RhinoError, Signal, Source};
use crate::stream::DDA_ERR_MAP;

#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::thread::sleep;
    use std::time::Duration;

    use clap::Parser;

    use crate::{Context, Resolution};
    use crate::filter::{DxColorConfig, DxColorFilterType};
    use crate::source::{DxDesktopDuplication, ScreenCapConfig};

    #[test]
    fn test_config() {
        let mut conf: ScreenCapConfig = "--screen 1 -r 1920x1080 -f 60.0".parse().unwrap();
        assert_eq!(conf, ScreenCapConfig {
            adapter: 0,
            screen: 1,
            refresh_rate: 60.0,
            resolution: Some(Resolution { width: 1920, height: 1080 }),
            skip_cursor: false,
        });
        let filter_conf = DxColorConfig::from_str("-c nv12 -r 1920x1080").unwrap();
        assert_eq!(filter_conf, DxColorConfig {
            resolution: Resolution { width: 1920, height: 1080 },
            color: DxColorFilterType::NV12Filter,
        })
    }

    #[test]
    fn test_screen_cap() {
        let conf = ScreenCapConfig::from_str("--screen 0 -r 1920x1080 -f 60.0").unwrap();
        let mut ctx = Context::None;
        let scp = DxDesktopDuplication::new(conf, &mut ctx).unwrap();
        sleep(Duration::from_millis(3000));
    }
}


#[derive(Parser, Debug, Clone, PartialEq)]
#[command(author, version, about, no_binary_name = true)]
pub struct ScreenCapConfig {
    /// index of the screen to capture
    #[arg(short = 'a', long, value_parser, default_value_t = 0)]
    pub adapter: u32,

    /// index of the screen to capture
    #[arg(short = 's', long, value_parser, default_value_t = 0)]
    pub screen: u32,

    /// frame rate to output
    #[arg(short = 'f', long, value_parser, default_value_t = 0f32)]
    pub refresh_rate: f32,

    /// resolution to set for the display
    #[arg(short = 'r', long, value_parser = Resolution::from_str)]
    pub resolution: Option<Resolution>,

    /// The screen will be captured without cursor.
    #[arg(long, action = ArgAction::Set,
    default_value_t = false,
    default_missing_value = "true",
    num_args = 0..=1,
    require_equals = false)]
    pub skip_cursor: bool,
}

impl FromStr for ScreenCapConfig {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let result = Self::try_parse_from(s.split(" ").filter(|x| { (*x).trim().len() != 0 }));
        return match result {
            Err(e) => {
                Err(format!("{:?}", e).to_owned())
            }
            Ok(parsed) => { Ok(parsed) }
        };
    }
}

pub struct DxDesktopDuplication {
    config: ScreenCapConfig,
    dupl: DesktopDuplicationApi,

    orig_mode: DisplayMode,
    curr_mode: DisplayMode,

    adapter: Adapter,
    display: Display,

    started: bool,
}

impl DxDesktopDuplication {
    pub fn new(conf: ScreenCapConfig, ctx: &mut Context) -> Result<Self> {
        set_process_dpi_awareness();
        co_init();
        let (adapter, display) = Self::get_adapter_output(conf.adapter, conf.screen)?;
        let orig_mode = display.get_current_display_mode().map_err(|e| { RhinoError::Unexpected(format!("{:?}", e)) })?;
        let mut curr_mode = orig_mode.clone();
        if conf.resolution.is_some() {
            let res = conf.resolution.as_ref().unwrap();
            curr_mode.width = res.width;
            curr_mode.height = res.height;
        }
        if conf.refresh_rate != 0f32 {
            curr_mode.refresh_num = (conf.refresh_rate * 1000f32) as u32;
            curr_mode.refresh_den = 1000;
        }
        display.set_display_mode(&curr_mode).map_err(|e| { RhinoError::Unexpected(format!("change display mode failed {:?}", e)) })?;
        curr_mode = display.get_current_display_mode().map_err(|e| { RhinoError::Unexpected(format!("{:?}", e)) })?;

        let mut dupl = DesktopDuplicationApi::new(adapter.clone(), display.clone()).map_err(DDA_ERR_MAP)?;
        dupl.configure(DuplicationApiOptions { skip_cursor: conf.skip_cursor, ..Default::default() });
        let (dev, dev_ctx) = dupl.get_device_and_ctx();
        *ctx = Context::DxContext(DxContext { device: dev, ctx: dev_ctx });

        Ok(Self {
            config: conf,
            dupl,
            orig_mode,
            curr_mode,
            adapter,
            display,
            started: false,
        })
    }

    fn get_adapter_output(adapter_idx: u32, screen_idx: u32) -> Result<(Adapter, Display)> {
        let adapters = AdapterFactory::new();
        let adapter = adapters.get_adapter_by_idx(adapter_idx);
        if let Some(adapter) = adapter {
            let screen = adapter.get_display_by_idx(screen_idx);
            if let Some(screen) = screen {
                Ok((adapter.clone(), screen.clone()))
            } else {
                Err(RhinoError::NotFound)
            }
        } else {
            Err(RhinoError::NotFound)
        }
    }
}

impl Drop for DxDesktopDuplication {
    fn drop(&mut self) {
        let _ = self.display.set_display_mode(&self.orig_mode);
    }
}

impl Signal for DxDesktopDuplication {
    fn signal(&mut self, _flags: u32) -> Result<()> {
        return Ok(());
    }
}

impl Config for DxDesktopDuplication {
    type ConfigType = ScreenCapConfig;

    fn configure(&mut self, c: Self::ConfigType) -> Result<()> {
        if c.resolution.is_some() {
            let res = c.resolution.as_ref().unwrap();
            self.curr_mode.height = res.height;
            self.curr_mode.width = res.width;
            if c.refresh_rate != 0f32 {
                self.curr_mode.refresh_num = (c.refresh_rate * 1000f32) as _;
                self.curr_mode.refresh_den = 1000;
            }
            self.display.set_display_mode(&self.curr_mode).map_err(DDA_ERR_MAP)?;
            self.curr_mode = self.display.get_current_display_mode().map_err(DDA_ERR_MAP)?;
        }
        self.dupl.configure(DuplicationApiOptions { skip_cursor: c.skip_cursor });
        return Ok(());
    }
}


impl Stream for DxDesktopDuplication {
    type Item = Result<Frame>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
        if !self.started {
            set_thread_priority();
            let (dev, _) = self.dupl.get_device_and_ctx();
            let dev: IDXGIDevice4 = dev.cast().unwrap();
            unsafe { dev.SetGPUThreadPriority(7); }
            set_gpu_priority();
            self.started = true;
        }
        let task = self.dupl.acquire_next_vsync_frame();
        futures::pin_mut!(task);
        let res = task.poll(cx);
        match res {
            Poll::Ready(result) => {
                let res = match result {
                    Ok(tex) => {
                        Ok(Frame::new(FrameType::Dx11Frame(tex)))
                    }
                    Err(e) => {
                        match e {
                            DDApiError::AccessLost | DDApiError::AccessDenied => {
                                Err(RhinoError::Recoverable(format!("Recoverable Error: `{:?}`", e)))
                            }
                            _ => {
                                Err(RhinoError::UnRecoverable(format!("Unreacoverable error `{:?}`", e)))
                            }
                        }
                    }
                };

                return Poll::Ready(Some(res));
            }
            Poll::Pending => {
                Poll::Pending
            }
        }
    }
}

impl Source for DxDesktopDuplication {}


pub fn set_thread_priority() {
    unsafe {
        let status = SetPriorityClass(GetCurrentProcess(), REALTIME_PRIORITY_CLASS);
        if status.is_err() {
            SetPriorityClass(GetCurrentProcess(), HIGH_PRIORITY_CLASS);
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST).unwrap();
        } else {
            info!("Process is set to time critical");
            SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_TIME_CRITICAL).unwrap();
        }
    }
}

pub fn set_gpu_priority() {
    unsafe {
        let process = GetCurrentProcess();
        let status = D3DKMTSetProcessSchedulingPriorityClass(process.0 as *mut c_void, 5);
        if status != 0 {
            warn!("cant set realtime gpu priority!!!, {:#x}",status);
            D3DKMTSetProcessSchedulingPriorityClass(process.0 as *mut c_void, 4);
        } else {
            info!("successfully set GPU Priority");
        }
    }
}


extern "system" {
    fn D3DKMTSetProcessSchedulingPriorityClass(handle: *mut c_void, priority: u32) -> u32;
}
