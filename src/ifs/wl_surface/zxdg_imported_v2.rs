use {
    crate::{
        client::{Client, ClientError},
        ifs::wl_surface::WlSurface,
        leaks::Tracker,
        object::{Object, Version},
        wire::{zxdg_imported_v2::*, WlSurfaceId, ZxdgImportedV2Id},
    },
    std::rc::Rc,
    thiserror::Error,
};

pub struct ZxdgImportedV2 {
    pub id: ZxdgImportedV2Id,
    pub client: Rc<Client>,
    pub surface: Option<Rc<WlSurface>>,
    pub tracker: Tracker<Self>,
    pub version: Version,
}

impl ZxdgImportedV2 {
    pub fn send_destroyed(&self) {
        self.client.event(Destroyed { self_id: self.id });
    }
}

impl ZxdgImportedV2RequestHandler for ZxdgImportedV2 {
    type Error = ZxdgImportedV2Error;

    fn destroy(&self, _req: Destroy, _slf: &Rc<Self>) -> Result<(), Self::Error> {
        self.client.remove_obj(self)?;
        // TODO: Invalidate any relationships
        Ok(())
    }

    fn set_parent_of(&self, req: SetParentOf, _slf: &Rc<Self>) -> Result<(), Self::Error> {
        let child_surface = self.client.lookup(req.surface)?;
        if child_surface.get_toplevel().is_none() {
            return Err(ZxdgImportedV2Error::InvalidSurface(child_surface.id));
        }
        if let Some(parent_surface) = &self.surface {
            if let Some(child_tl) = child_surface.get_toplevel() {
                if let Some(parent_tl) = parent_surface.get_toplevel() {
                    if let Some(parent_node) = parent_tl.node_into_containing_node() {
                        child_tl.tl_set_parent(parent_node);
                    }
                }
            }
        }
        Ok(())
    }
}

object_base! {
    self = ZxdgImportedV2;
    version = self.version;
}

impl Object for ZxdgImportedV2 {
    fn break_loops(&self) {
        // self.deactivate();
    }
}

simple_add_obj!(ZxdgImportedV2);

#[derive(Debug, Error)]
pub enum ZxdgImportedV2Error {
    #[error(transparent)]
    ClientError(Box<ClientError>),
    #[error("Surface {0} is not a toplevel")]
    InvalidSurface(WlSurfaceId),
}
efrom!(ZxdgImportedV2Error, ClientError);
