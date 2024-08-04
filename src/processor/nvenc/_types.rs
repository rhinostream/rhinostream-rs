use std::str::FromStr;

use clap::{Parser, ValueEnum};
use win_desktop_duplication::texture::ColorFormat;

use nvenc_sys as nvenc;
use nvenc_sys::GUID;

use crate::Resolution;

#[derive(Parser, Debug, Clone, PartialEq)]
#[command(author, version, about)]
#[command(no_binary_name = true)]
pub struct NvencConfig {
    #[arg(short = 'p', long, value_enum, default_value = "p4")]
    pub preset: Preset,

    #[arg(long, value_enum, default_value = "auto")]
    pub profile: Profile,

    #[arg(long)]
    pub level: Option<u32>,

    #[arg(long, value_enum, default_value = "disabled")]
    pub multi_pass: MultiPass,

    #[arg(short = 't', long, value_enum, default_value = "low_latency")]
    pub tuning_info: TuningInfo,

    #[arg(long, value_enum, default_value = "argb")]
    pub color: NvencColor,

    #[arg(short = 'c', long, value_enum, default_value = "h264")]
    pub codec: NvencCodec,

    #[arg(long, value_parser, default_value = "disabled")]
    pub aq: AdaptiveQuantization,

    #[arg(short = 'r', long, value_parser = Resolution::from_str, default_value = "0x0")]
    pub resolution: Resolution,

    #[arg(short = 'b', long, value_parser, default_value_t = 5_000_000)]
    pub bitrate: u64,

    #[arg(short = 'f', long, value_parser, default_value_t = 60f32)]
    pub framerate: f32,
}

impl FromStr for NvencConfig {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        let result = Self::try_parse_from(s.split(" ").filter(|x| { (*x).trim().len() != 0 }));
        return match result {
            Err(e) => {
                Err(format!("{:?}", e))
            }
            Ok(parsed) => { Ok(parsed) }
        };
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum AdaptiveQuantization {
    Disabled,
    Spacial,
    Temporal,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum Preset {
    P1,
    P2,
    P3,
    P4,
    P5,
    P6,
    P7,
}

impl Into<GUID> for Preset {
    fn into(self) -> GUID {
        match self {
            Preset::P1 => { unsafe { nvenc::NV_ENC_PRESET_P1_GUID } }
            Preset::P2 => { unsafe { nvenc::NV_ENC_PRESET_P2_GUID } }
            Preset::P3 => { unsafe { nvenc::NV_ENC_PRESET_P3_GUID } }
            Preset::P4 => { unsafe { nvenc::NV_ENC_PRESET_P4_GUID } }
            Preset::P5 => { unsafe { nvenc::NV_ENC_PRESET_P5_GUID } }
            Preset::P6 => { unsafe { nvenc::NV_ENC_PRESET_P6_GUID } }
            Preset::P7 => { unsafe { nvenc::NV_ENC_PRESET_P7_GUID } }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum Profile {
    Auto,
    Constrained,
    Baseline,
    Main,
    High,
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum NvencColor {
    ARGB,
    ABGR,
    AYUV,
    YUV444,
    YUV420,
    NV12,

    YUV444_10bit,
    ARGB_10bit,
    ABGR_10bit,
    YUV420_10bit,
}

impl Into<nvenc::NV_ENC_BUFFER_FORMAT> for NvencColor {
    fn into(self) -> nvenc::NV_ENC_BUFFER_FORMAT {
        match self {
            NvencColor::ARGB => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_ARGB }
            NvencColor::ABGR => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_ABGR }
            NvencColor::AYUV => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_AYUV }
            NvencColor::YUV444 => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_YUV444 }
            NvencColor::YUV420 => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_IYUV }
            NvencColor::NV12 => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_NV12 }
            NvencColor::YUV444_10bit => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_YUV444_10BIT }
            NvencColor::ARGB_10bit => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_ARGB10 }
            NvencColor::ABGR_10bit => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_ABGR10 }
            NvencColor::YUV420_10bit => { nvenc::_NV_ENC_BUFFER_FORMAT_NV_ENC_BUFFER_FORMAT_YUV420_10BIT }
        }
    }
}

impl From<ColorFormat> for NvencColor {
    fn from(f: ColorFormat) -> Self {
        match f {
            ColorFormat::Unknown => { NvencColor::ARGB }
            ColorFormat::ARGB8UNorm => { NvencColor::ARGB }
            ColorFormat::ABGR8UNorm => { NvencColor::ABGR }
            ColorFormat::YUV444 => { NvencColor::YUV444 }
            ColorFormat::AYUV => { NvencColor::AYUV }
            ColorFormat::YUV420 => { NvencColor::YUV420 }
            ColorFormat::NV12 => { NvencColor::NV12 }
            ColorFormat::ARGB16Float => { NvencColor::ARGB_10bit }
            ColorFormat::ARGB10UNorm => { NvencColor::ARGB_10bit }
            ColorFormat::Y410 => { unimplemented!() }
            ColorFormat::YUV444_10bit => { NvencColor::YUV444_10bit }
            ColorFormat::YUV420_10bit => { NvencColor::YUV420_10bit }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum NvencCodec {
    H264,
    HEVC,
}

impl Into<GUID> for NvencCodec {
    fn into(self) -> GUID {
        match self {
            NvencCodec::H264 => {
                unsafe { nvenc::NV_ENC_CODEC_H264_GUID }
            }
            NvencCodec::HEVC => {
                unsafe { nvenc::NV_ENC_CODEC_HEVC_GUID }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum MultiPass {
    Disabled,
    QuarterRes,
    FullRes,
}

impl Into<nvenc::NV_ENC_MULTI_PASS> for MultiPass {
    fn into(self) -> nvenc::NV_ENC_MULTI_PASS {
        match self {
            MultiPass::Disabled => { unsafe { nvenc::_NV_ENC_MULTI_PASS_NV_ENC_MULTI_PASS_DISABLED } }
            MultiPass::QuarterRes => { unsafe { nvenc::_NV_ENC_MULTI_PASS_NV_ENC_TWO_PASS_QUARTER_RESOLUTION } }
            MultiPass::FullRes => { unsafe { nvenc::_NV_ENC_MULTI_PASS_NV_ENC_TWO_PASS_FULL_RESOLUTION } }
        }
    }
}


#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum TuningInfo {
    LowLatency,
    UltraLowLatency,
}

impl Into<nvenc::NV_ENC_TUNING_INFO> for TuningInfo {
    fn into(self) -> nvenc::NV_ENC_TUNING_INFO {
        match self {
            TuningInfo::LowLatency => { unsafe { nvenc::NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_LOW_LATENCY } }
            TuningInfo::UltraLowLatency => { unsafe { nvenc::NV_ENC_TUNING_INFO_NV_ENC_TUNING_INFO_ULTRA_LOW_LATENCY } }
        }
    }
}
