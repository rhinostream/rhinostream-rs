#[cfg(test)]
mod test {
    use std::str::FromStr;
    use std::thread::{sleep};
    use std::time::Duration;
    use crate::{Context, Resolution};
    use crate::filter::DxColorConfig;
    use crate::source::{ScreenCap, ScreenCapConfig};

    #[test]
    fn test_config() {
        let conf: ScreenCapConfig = "--screen 1 -r 1920x1080 -f 60.0".parse().unwrap();
        assert_eq!(conf, ScreenCapConfig {
            screen: 1,
            refresh_rate: 60.0,
            resolution: Some(Resolution { width: 1920, height: 1080 }),
        });

        let filter_conf = DxColorConfig::from_str("-r 1920x1080").unwrap();
        assert_eq!(filter_conf, DxColorConfig {
            resolution: Resolution { width: 1920, height: 1080 }
        })
    }

    #[test]
    fn test_screen_cap() {
        let conf = ScreenCapConfig::from_str("--screen 0 -r 1920x1080 -f 60.0").unwrap();
        let mut ctx = Context::None;
        let scp = ScreenCap::new(conf, &mut ctx).unwrap();
        sleep(Duration::from_millis(3000));
    }
}


use std::mem::swap;
use std::pin::Pin;
use std::str::FromStr;
use std::task::Poll;
use clap::{Parser, AppSettings};
use futures::{select, Stream, FutureExt, Future};
use tokio::sync::mpsc::{channel, Receiver, Sender};
use win_desktop_duplication::{co_init, DesktopDuplicationApi, set_process_dpi_awareness};
use win_desktop_duplication::devices::{Adapter, AdapterFactory};
use win_desktop_duplication::errors::DDApiError;
use win_desktop_duplication::outputs::{Display, DisplayMode};
use crate::{Config, Context, FrameType, DxContext, Frame, Resolution, Result, RhinoError, Signal, Source};
use crate::stream::DDA_ERR_MAP;


#[derive(Parser, Debug, Clone, PartialEq)]
#[clap(author, version, about)]
#[clap(setting(AppSettings::NoBinaryName))]
pub struct ScreenCapConfig {
    /// index of the screen to capture
    #[clap(short = 's', long, value_parser, default_value_t = 0)]
    pub screen: u8,

    /// frame rate to output
    #[clap(short = 'f', long, value_parser, default_value_t = 0f32)]
    pub refresh_rate: f32,

    /// resolution to set for the display
    #[clap(short = 'r', long, value_parser = Resolution::from_str)]
    pub resolution: Option<Resolution>,
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

pub struct ScreenCap {
    config: ScreenCapConfig,
    dupl: DesktopDuplicationApi,

    orig_mode: DisplayMode,
    curr_mode: DisplayMode,

    adapter: Adapter,
    display: Display,

}

impl ScreenCap {
    pub fn new(conf: ScreenCapConfig, ctx: &mut Context) -> Result<Self> {
        set_process_dpi_awareness();
        co_init();
        let (adapter, display) = Self::get_adapter_output(conf.screen)?;
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

        let dupl = DesktopDuplicationApi::new(adapter.clone(), display.clone()).map_err(DDA_ERR_MAP)?;
        let (dev, dev_ctx) = dupl.get_device_and_ctx();
        *ctx = Context::DxContext(DxContext { device: dev, ctx: dev_ctx });

        Ok(Self {
            config: conf,
            dupl,
            orig_mode,
            curr_mode,
            adapter,
            display,
        })
    }

    fn get_adapter_output(screen: u8) -> Result<(Adapter, Display)> {
        let mut idx = 0;
        for adapter in AdapterFactory::new() {
            for output in adapter.iter_displays() {
                if idx == screen {
                    return Ok((adapter, output));
                }
                idx += 1;
            }
        }
        return Err(RhinoError::NotFound);
    }
}

impl Drop for ScreenCap {
    fn drop(&mut self) {
        let _ = self.display.set_display_mode(&self.orig_mode);
    }
}

impl Signal for ScreenCap {
    fn signal(&mut self, _flags: u32) -> Result<()> {
        return Ok(());
    }
}

impl Config for ScreenCap {
    type ConfigType = ScreenCapConfig;

    fn configure(&mut self, c: Self::ConfigType) -> Result<()> {
        if c.resolution.is_some() {
            let res = c.resolution.as_ref().unwrap();
            self.curr_mode.height = res.height;
            self.curr_mode.width = res.width;
        }
        if c.refresh_rate != 0f32 {
            self.curr_mode.refresh_num = (c.refresh_rate * 1000f32) as _;
            self.curr_mode.refresh_den = 1000;
        }
        self.display.set_display_mode(&self.curr_mode).map_err(DDA_ERR_MAP)?;
        self.curr_mode = self.display.get_current_display_mode().map_err(DDA_ERR_MAP)?;
        return Ok(());
    }
}


impl Stream for ScreenCap {
    type Item = Result<Frame>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Option<Self::Item>> {
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

impl Source for ScreenCap {}

