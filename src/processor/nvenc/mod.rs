use std::ffi::c_void;
use std::mem::transmute;
use std::ptr::null_mut;
use log::error;
use windows::Win32::Graphics::Direct3D11::ID3D11Device4;
use crate::{Context, Result, RhinoError};
use nvenc_sys as nvenc;

struct NvEnc {
    nv: nvenc::NV_ENCODE_API_FUNCTION_LIST,
    enc: *mut c_void,
}

fn into_err(status: nvenc::NVENCSTATUS) -> Result<()> {
    match status {
        nvenc::_NVENCSTATUS_NV_ENC_SUCCESS => Ok(()),
        _ => Err(RhinoError::Unexpected(format!("Nvenc Status: {}", status)))
    }
}

impl NvEnc {
    pub fn new(&mut ctx: Context) -> Result<Self> {
        let nv = create_api_instance()?;

        let (dev, dev_ctx) = match ctx {
            Context::DxContext(dctx) => {
                Ok((dctx.device.clone(), dctx.ctx.clone()))
            }
            _ => {
                Err(RhinoError::UnSupported)
            }
        }?;
        let enc = open_nvenc_d3d_session(&nv, dev.clone())?;

        Ok(Self {
            nv,
            enc,
        })
    }
}


fn create_api_instance() -> Result<nvenc::NV_ENCODE_API_FUNCTION_LIST> {
    let mut nv: nvenc::NV_ENCODE_API_FUNCTION_LIST = Default::default();
    nv.version = nvenc::NV_ENCODE_API_FUNCTION_LIST_VER;
    unsafe { into_err(nvenc::NvEncodeAPICreateInstance(&mut nv))?; }
    Ok(nv)
}

fn open_nvenc_d3d_session(nv: &nvenc::NV_ENCODE_API_FUNCTION_LIST, device: ID3D11Device4) -> Result<*mut c_void> {
    // set init params
    let mut params: nvenc::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS = Default::default();
    params.version = nvenc::NV_ENC_OPEN_ENCODE_SESSION_EX_PARAMS_VER;
    params.apiVersion = nvenc::NVENCAPI_VERSION;
    unsafe { params.device = transmute(device); }
    params.deviceType = nvenc::_NV_ENC_DEVICE_TYPE_NV_ENC_DEVICE_TYPE_DIRECTX;
    let mut enc = null_mut();

    // call the open session function
    let func = nv.nvEncOpenEncodeSessionEx.as_ref().unwrap();
    let status = into_err((*func)(&mut params, &mut enc));
    if status.is_err() {
        error!("failed to open encode session. NvencStatus: {:?}",status);
        status?;
    }
    return Ok(enc);
}
