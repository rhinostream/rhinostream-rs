use std::ptr::null;
use std::str::FromStr;

use clap::{Parser, ValueEnum};
use dxfilter::{ConvertARGBToAYUV, ConvertARGBToNV12, DxFilter, ScaleARGBOrAYUV};
use log::error;
use win_desktop_duplication::texture::{ColorFormat, Texture};
use windows::Win32::Graphics::Direct3D11::{D3D11_BIND_FLAG, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT, ID3D11Device4, ID3D11DeviceContext4};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT, DXGI_SAMPLE_DESC};

use crate::{Config, Context, Filter, Frame, FrameType, Resolution, Result, RhinoError};
use crate::stream::{DXF_ERR_MAP, WIN_ERR_MAP};

#[derive(Parser, Debug, Clone, PartialEq)]
#[command(author, version, about, no_binary_name = true)]
pub struct DxColorConfig {
    /// used to determine the target color of the image.
    #[arg(short = 'c', long, value_parser, default_value = "rgb")]
    pub color: DxColorFilterType,

    #[arg(short = 'r', long, value_parser = Resolution::from_str, default_value = "1920x1080")]
    pub resolution: Resolution,
}

impl FromStr for DxColorConfig {
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

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, ValueEnum, Debug)]
#[repr(u8)]
pub enum DxColorFilterType {
    #[value(name = "nv12")]
    NV12Filter,
    #[value(name = "rgb")]
    RGBFilter,
    #[value(name = "ayuv")]
    AYUVFilter,
}

pub struct DxColor<T: DxFilter + 'static> {
    config: DxColorConfig,
    filter: T,
    input_tex: Texture,
    output_tex: Texture,

    device: ID3D11Device4,
    ctx: ID3D11DeviceContext4,
    out_format: ColorFormat,
}

fn create_texture(device: &ID3D11Device4, format: DXGI_FORMAT, c: &Resolution, bind: D3D11_BIND_FLAG) -> Result<Texture> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: c.width,
        Height: c.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as _,
        CPUAccessFlags: Default::default(),
        MiscFlags: Default::default(),
    };
    let mut tex = None;

    unsafe { device.CreateTexture2D(&desc, None, Some(&mut tex)) }.map_err(WIN_ERR_MAP)?;
    Ok(Texture::new(tex.unwrap()))
}

pub fn new_nv12_filter(conf: DxColorConfig, ctx: &mut Context) -> Result<DxColor<ConvertARGBToNV12>> {
    if !matches!(ctx,Context::DxContext(_)) {
        error!("context provided was not directx context");
        return Err(RhinoError::UnSupported);
    }
    let (device, dev_ctx) = match ctx {
        Context::DxContext(c) => {
            Ok((c.device.clone(), c.ctx.clone()))
        }
        _ => {
            error!("expected directx context got `{:?}`",ctx);
            Err(RhinoError::UnSupported)
        }
    }?;
    let input = create_texture(&device, ColorFormat::ARGB8UNorm.into(), &conf.resolution, D3D11_BIND_SHADER_RESOURCE)?;
    let output_tex = create_texture(&device, ColorFormat::NV12.into(), &conf.resolution, D3D11_BIND_RENDER_TARGET)?;
    Ok(DxColor {
        config: conf,
        filter: ConvertARGBToNV12::new(&input, &output_tex, &device).map_err(DXF_ERR_MAP)?,
        input_tex: input,
        output_tex,
        device,
        ctx: dev_ctx,
        out_format: ColorFormat::NV12,
    })
}

pub fn new_argb_filter(conf: DxColorConfig, ctx: &mut Context) -> Result<DxColor<ScaleARGBOrAYUV>> {
    if !matches!(ctx,Context::DxContext(_)) {
        error!("context provided was not directx context");
        return Err(RhinoError::UnSupported);
    }
    let (device, dev_ctx) = match ctx {
        Context::DxContext(c) => {
            Ok((c.device.clone(), c.ctx.clone()))
        }
        _ => {
            error!("expected directx context got `{:?}`",ctx);
            Err(RhinoError::UnSupported)
        }
    }?;
    let input = create_texture(&device, ColorFormat::ARGB8UNorm.into(), &conf.resolution, D3D11_BIND_SHADER_RESOURCE)?;
    let output_tex = create_texture(&device, ColorFormat::ARGB8UNorm.into(), &conf.resolution, D3D11_BIND_RENDER_TARGET)?;
    Ok(DxColor {
        config: conf,
        filter: ScaleARGBOrAYUV::new(&input, &output_tex, &device).map_err(DXF_ERR_MAP)?,
        input_tex: input,
        output_tex,
        device,
        ctx: dev_ctx,
        out_format: ColorFormat::ARGB8UNorm,
    })
}

pub fn new_ayuv_filter(conf: DxColorConfig, ctx: &mut Context) -> Result<DxColor<ConvertARGBToAYUV>> {
    if !matches!(ctx,Context::DxContext(_)) {
        error!("context provided was not directx context");
        return Err(RhinoError::UnSupported);
    }
    let (device, dev_ctx) = match ctx {
        Context::DxContext(c) => {
            Ok((c.device.clone(), c.ctx.clone()))
        }
        _ => {
            error!("expected directx context got `{:?}`",ctx);
            Err(RhinoError::UnSupported)
        }
    }?;
    let input = create_texture(&device, ColorFormat::ARGB8UNorm.into(), &conf.resolution, D3D11_BIND_SHADER_RESOURCE)?;
    let output_tex = create_texture(&device, ColorFormat::AYUV.into(), &conf.resolution, D3D11_BIND_RENDER_TARGET)?;
    Ok(DxColor {
        config: conf,
        filter: ConvertARGBToAYUV::new(&input, &output_tex, &device).map_err(DXF_ERR_MAP)?,
        input_tex: input,
        output_tex,
        device,
        ctx: dev_ctx,
        out_format: ColorFormat::AYUV,
    })
}

impl<T: DxFilter + 'static> Config for DxColor<T> {
    type ConfigType = DxColorConfig;

    fn configure(&mut self, c: Self::ConfigType) -> Result<()> {
        self.output_tex = create_texture(&self.device, self.out_format.into(), &c.resolution, D3D11_BIND_RENDER_TARGET)?;
        self.filter.set_output_tex(&self.output_tex)?;
        self.config = c;
        Ok(())
    }
}

unsafe impl<T: DxFilter + 'static> Sync for DxColor<T> {}

unsafe impl<T: DxFilter + 'static> Send for DxColor<T> {}

impl<T: DxFilter + 'static> Unpin for DxColor<T> {}

impl<T: DxFilter + 'static> Filter for DxColor<T> {
    fn apply_filter(&mut self, frame: &Frame) -> Result<Frame> {
        let tex = match &frame.data {
            FrameType::Dx11Frame(tex) => { Ok(tex) }
            _ => {
                Err(RhinoError::UnSupported)
            }
        }?;
        if tex.desc() != self.input_tex.desc() {
            self.input_tex = create_texture(&self.device, tex.desc().format.into(), &Resolution { width: tex.desc().width, height: tex.desc().height }, D3D11_BIND_SHADER_RESOURCE)?;
            self.filter.set_input_tex(&self.input_tex)?;
        }
        unsafe { self.ctx.CopyResource(self.input_tex.as_raw_ref(), tex.as_raw_ref()); }
        self.filter.apply_filter(&self.ctx).map_err(DXF_ERR_MAP)?;
        Ok(Frame::new_from(frame, FrameType::Dx11Frame(self.output_tex.clone())))
    }
}