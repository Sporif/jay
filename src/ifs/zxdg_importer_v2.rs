use {
    crate::{
        client::{Client, ClientError},
        globals::{Global, GlobalName},
        ifs::wl_surface::zxdg_imported_v2::{ZxdgImportedV2, ZxdgImportedV2Error},
        leaks::Tracker,
        object::{Object, Version},
        utils::errorfmt::ErrorFmt,
        wire::{zxdg_importer_v2::*, ZxdgImporterV2Id},
    },
    std::rc::Rc,
    thiserror::Error,
};

pub struct ZxdgImporterV2Global {
    name: GlobalName,
}

impl ZxdgImporterV2Global {
    pub fn new(name: GlobalName) -> Self {
        Self { name }
    }

    fn bind_(
        self: Rc<Self>,
        id: ZxdgImporterV2Id,
        client: &Rc<Client>,
        version: Version,
    ) -> Result<(), ZxdgImporterV2Error> {
        let obj = Rc::new(ZxdgImporterV2 {
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

global_base!(ZxdgImporterV2Global, ZxdgImporterV2, ZxdgImporterV2Error);

impl Global for ZxdgImporterV2Global {
    fn singleton(&self) -> bool {
        true
    }

    fn version(&self) -> u32 {
        1
    }
}

simple_add_global!(ZxdgImporterV2Global);

pub struct ZxdgImporterV2 {
    pub id: ZxdgImporterV2Id,
    pub client: Rc<Client>,
    pub tracker: Tracker<Self>,
    pub version: Version,
}

impl ZxdgImporterV2RequestHandler for ZxdgImporterV2 {
    type Error = ZxdgImporterV2Error;

    fn destroy(&self, _req: Destroy, _slf: &Rc<Self>) -> Result<(), Self::Error> {
        self.client.remove_obj(self)?;
        Ok(())
    }

    fn import_toplevel(&self, req: ImportToplevel, _slf: &Rc<Self>) -> Result<(), Self::Error> {
        let send_destroyed = || {
            let imported = ZxdgImportedV2 {
                id: req.id,
                client: self.client.clone(),
                surface: None,
                tracker: Default::default(),
                version: self.version,
            };
            imported.send_destroyed();
        };
        let handle = match req.handle.parse() {
            Ok(t) => t,
            Err(e) => {
                log::warn!("Could not parse exported surface handle: {}", ErrorFmt(e));
                send_destroyed();
                return Ok(());
            }
        };
        if let Some(surface) = self.client.state.foreign_exports.get(&handle) {
            let obj = Rc::new(ZxdgImportedV2 {
                id: req.id,
                client: self.client.clone(),
                surface: Some(surface),
                tracker: Default::default(),
                version: self.version,
            });
            track!(self.client, obj);
            self.client.add_client_obj(&obj)?;
        } else {
            send_destroyed();
        }
        Ok(())
    }
}

object_base! {
    self = ZxdgImporterV2;
    version = self.version;
}

impl Object for ZxdgImporterV2 {}

simple_add_obj!(ZxdgImporterV2);

#[derive(Debug, Error)]
pub enum ZxdgImporterV2Error {
    #[error(transparent)]
    ClientError(Box<ClientError>),
    #[error(transparent)]
    ZxdgImportedV2Error(#[from] ZxdgImportedV2Error),
}
efrom!(ZxdgImporterV2Error, ClientError);
