use {
    crate::{
        client::{Client, ClientError},
        leaks::Tracker,
        object::{Object, Version},
        wire::{zxdg_exported_v2::*, ZxdgExportedV2Id},
    },
    std::rc::Rc,
    thiserror::Error,
};

pub struct ZxdgExportedV2 {
    pub id: ZxdgExportedV2Id,
    pub client: Rc<Client>,
    pub handle: crate::utils::opaque::Opaque,
    pub tracker: Tracker<Self>,
    pub version: Version,
}

impl ZxdgExportedV2 {
    pub fn send_handle(&self) {
        self.client.event(Handle {
            self_id: self.id,
            handle: &self.handle.to_string(),
        });
    }
}

impl ZxdgExportedV2RequestHandler for ZxdgExportedV2 {
    type Error = ZxdgExportedV2Error;

    fn destroy(&self, _req: Destroy, _slf: &Rc<Self>) -> Result<(), Self::Error> {
        self.client.remove_obj(self)?;
        self.client.state.foreign_exports.remove(&self.handle);
        // TODO: Invalidate any relationships
        Ok(())
    }
}

object_base! {
    self = ZxdgExportedV2;
    version = self.version;
}

impl Object for ZxdgExportedV2 {
    fn break_loops(&self) {
        // self.deactivate();
    }
}

simple_add_obj!(ZxdgExportedV2);

#[derive(Debug, Error)]
pub enum ZxdgExportedV2Error {
    #[error(transparent)]
    ClientError(Box<ClientError>),
}
efrom!(ZxdgExportedV2Error, ClientError);
