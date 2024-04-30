use {
    crate::{
        client::{Client, ClientError},
        globals::{Global, GlobalName},
        ifs::wl_surface::zxdg_exported_v2::{ZxdgExportedV2, ZxdgExportedV2Error},
        leaks::Tracker,
        object::{Object, Version},
        wire::{zxdg_exporter_v2::*, WlSurfaceId, ZxdgExporterV2Id},
    },
    std::rc::Rc,
    thiserror::Error,
};

pub struct ZxdgExporterV2Global {
    name: GlobalName,
}

impl ZxdgExporterV2Global {
    pub fn new(name: GlobalName) -> Self {
        Self { name }
    }

    fn bind_(
        self: Rc<Self>,
        id: ZxdgExporterV2Id,
        client: &Rc<Client>,
        version: Version,
    ) -> Result<(), ZxdgExporterV2Error> {
        let obj = Rc::new(ZxdgExporterV2 {
            id,
            client: client.clone(),
            tracker: Default::default(),
            version,
        });
        track!(client, obj);
        client.add_client_obj(&obj)?;
        Ok(())
    }
}

global_base!(ZxdgExporterV2Global, ZxdgExporterV2, ZxdgExporterV2Error);

impl Global for ZxdgExporterV2Global {
    fn singleton(&self) -> bool {
        true
    }

    fn version(&self) -> u32 {
        1
    }
}

simple_add_global!(ZxdgExporterV2Global);

pub struct ZxdgExporterV2 {
    pub id: ZxdgExporterV2Id,
    pub client: Rc<Client>,
    pub tracker: Tracker<Self>,
    pub version: Version,
}

impl ZxdgExporterV2RequestHandler for ZxdgExporterV2 {
    type Error = ZxdgExporterV2Error;

    fn destroy(&self, _req: Destroy, _slf: &Rc<Self>) -> Result<(), Self::Error> {
        self.client.remove_obj(self)?;
        Ok(())
    }

    fn export_toplevel(&self, req: ExportToplevel, _slf: &Rc<Self>) -> Result<(), Self::Error> {
        let surface = self.client.lookup(req.surface)?;
        if surface.get_toplevel().is_none() {
            return Err(ZxdgExporterV2Error::InvalidSurface(surface.id));
        }
        let handle = crate::utils::opaque::opaque();
        self.client.state.foreign_exports.set(handle, surface);
        let obj = Rc::new(ZxdgExportedV2 {
            id: req.id,
            client: self.client.clone(),
            handle,
            tracker: Default::default(),
            version: self.version,
        });
        track!(self.client, obj);
        self.client.add_client_obj(&obj)?;
        obj.send_handle();
        Ok(())
    }
}

object_base! {
    self = ZxdgExporterV2;
    version = self.version;
}

impl Object for ZxdgExporterV2 {}

simple_add_obj!(ZxdgExporterV2);

#[derive(Debug, Error)]
pub enum ZxdgExporterV2Error {
    #[error(transparent)]
    ClientError(Box<ClientError>),
    #[error("Surface {0} is not a toplevel")]
    InvalidSurface(WlSurfaceId),
    #[error(transparent)]
    ZxdgExportedV2Error(#[from] ZxdgExportedV2Error),
}
efrom!(ZxdgExporterV2Error, ClientError);
