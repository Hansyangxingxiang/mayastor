use std::{
    ffi::CString,
    fmt::{Debug, Display, Formatter},
    ops::{Deref, DerefMut},
};

use futures::channel::oneshot;
use nix::errno::Errno;
use once_cell::sync::Lazy;

use spdk_rs::{
    ffihelper::{copy_cstr_with_null, copy_str_with_null},
    libspdk::{
        spdk_nvme_transport_id,
        spdk_nvmf_tgt_add_transport,
        spdk_nvmf_transport_create,
        SPDK_NVME_TRANSPORT_TCP,
        SPDK_NVMF_ADRFAM_IPV4,
        SPDK_NVMF_TRSVCID_MAX_LEN,
    },
};

use crate::{
    core::MayastorEnvironment,
    ffihelper::{cb_arg, done_errno_cb, AsStr, ErrnoResult, FfiResult},
    subsys::{
        nvmf::{Error, NVMF_TGT},
        Config,
    },
};

static TCP_TRANSPORT: Lazy<CString> =
    Lazy::new(|| CString::new("TCP").unwrap());

pub async fn add_tcp_transport() -> Result<(), Error> {
    let cfg = Config::get();
    let mut opts = cfg.nvmf_tgt_conf.opts.into();
    let transport = unsafe {
        spdk_nvmf_transport_create(TCP_TRANSPORT.as_ptr(), &mut opts)
    };

    transport.to_result(|_| Error::Transport {
        source: Errno::UnknownErrno,
        msg: "failed to create transport".into(),
    })?;

    let (s, r) = oneshot::channel::<ErrnoResult<()>>();
    unsafe {
        NVMF_TGT.with(|t| {
            spdk_nvmf_tgt_add_transport(
                t.borrow().tgt.as_ptr(),
                transport,
                Some(done_errno_cb),
                cb_arg(s),
            );
        })
    };

    let _result = r.await.unwrap();

    debug!("Added TCP nvmf transport");
    Ok(())
}

pub struct TransportId(pub(crate) spdk_nvme_transport_id);
impl Deref for TransportId {
    type Target = spdk_nvme_transport_id;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for TransportId {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl TransportId {
    pub fn new(port: u16) -> Self {
        let address = get_ipv4_address().unwrap();

        let mut trid = spdk_nvme_transport_id {
            trtype: SPDK_NVME_TRANSPORT_TCP,
            adrfam: SPDK_NVMF_ADRFAM_IPV4,
            ..Default::default()
        };

        let port = format!("{port}");
        assert!(port.len() < SPDK_NVMF_TRSVCID_MAX_LEN as usize);

        copy_cstr_with_null(&TCP_TRANSPORT, &mut trid.trstring);
        copy_str_with_null(&address, &mut trid.traddr);
        copy_str_with_null(&port, &mut trid.trsvcid);

        Self(trid)
    }

    pub fn as_ptr(&self) -> *mut spdk_nvme_transport_id {
        &self.0 as *const _ as *mut spdk_nvme_transport_id
    }
}

impl Display for TransportId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "nvmf://{}:{}",
            self.0.traddr.as_str(),
            self.0.trsvcid.as_str()
        )
    }
}

impl Debug for TransportId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Transport ID")
            .field("trtype", &self.0.trtype)
            .field("trstring", &self.0.trstring.as_str().to_string())
            .field("traddr", &self.0.traddr.as_str().to_string())
            .field("trsvcid", &self.0.trsvcid.as_str().to_string())
            .finish()
    }
}

pub(crate) fn get_ipv4_address() -> Result<String, Error> {
    match MayastorEnvironment::get_nvmf_tgt_ip() {
        Ok(val) => Ok(val),
        Err(msg) => Err(Error::CreateTarget {
            msg,
        }),
    }
}
