//! The fx service as the runtime has it: a service file (`fx.dks`), not a row of the image. A host test of the DSP section, the effect rows or the attribution of
//! an effect needs its row: this adds it to the table as `Registry::add` does at the commit of the loaded file, and takes it out as `Registry::remove` does at the unload.
//! Take the test lock first (`test_lock::LOCK`): the row table is a static.
use crate::core_api::RealCore;
use crate::svc_rows;
use dkrt_abi::service::ServiceRow;
use dkrt_sdk::records::KINDS_FX;
use dkrt_sdk::OK;
use dkrt_svc_fx::records::Fx;

/// The module record of the service file in these tests.
const FILE_REC: usize = 7;

pub(crate) struct LoadedFx;

impl LoadedFx {
    pub(crate) fn new() -> LoadedFx {
        static ROW: ServiceRow = ServiceRow::of::<Fx, RealCore>("fx", &KINDS_FX);
        assert_eq!(unsafe { svc_rows::add(FILE_REC, &ROW) }, OK);
        LoadedFx
    }
}

impl Drop for LoadedFx {
    fn drop(&mut self) {
        unsafe { svc_rows::remove(FILE_REC) };
    }
}
