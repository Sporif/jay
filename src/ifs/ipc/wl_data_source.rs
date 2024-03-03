use {
    crate::{
        client::{Client, ClientError},
        ifs::{
            ipc::{
                add_data_source_mime_type, break_source_loops, cancel_offers, destroy_data_source,
                wl_data_device::ClipboardIpc,
                wl_data_device_manager::{DND_ALL, DND_NONE},
                wl_data_offer::WlDataOffer,
                SharedState, SourceData, OFFER_STATE_ACCEPTED, OFFER_STATE_DROPPED,
                SOURCE_STATE_CANCELLED, SOURCE_STATE_DROPPED,
            },
            wl_seat::WlSeatGlobal,
            xdg_toplevel_drag_v1::XdgToplevelDragV1,
        },
        leaks::Tracker,
        object::Object,
        utils::{
            bitflags::BitflagsExt,
            buffd::{MsgParser, MsgParserError},
            cell_ext::CellExt,
            clonecell::CloneCell,
        },
        wire::{wl_data_source::*, WlDataSourceId},
        xwayland::XWaylandEvent,
    },
    std::rc::Rc,
    thiserror::Error,
    uapi::OwnedFd,
};

#[allow(dead_code)]
const INVALID_ACTION_MASK: u32 = 0;
#[allow(dead_code)]
const INVALID_SOURCE: u32 = 1;

pub struct WlDataSource {
    pub id: WlDataSourceId,
    pub data: SourceData<ClipboardIpc>,
    pub version: u32,
    pub tracker: Tracker<Self>,
    pub toplevel_drag: CloneCell<Option<Rc<XdgToplevelDragV1>>>,
}

impl WlDataSource {
    pub fn new(id: WlDataSourceId, client: &Rc<Client>, is_xwm: bool, version: u32) -> Self {
        Self {
            id,
            tracker: Default::default(),
            data: SourceData::new(client, is_xwm),
            version,
            toplevel_drag: Default::default(),
        }
    }

    pub fn on_leave(&self) {
        if self
            .data
            .shared
            .get()
            .state
            .get()
            .contains(OFFER_STATE_DROPPED)
        {
            return;
        }
        self.data.shared.set(Rc::new(SharedState::default()));
        self.send_target(None);
        self.send_action(DND_NONE);
        cancel_offers::<ClipboardIpc>(self);
    }

    pub fn update_selected_action(&self) {
        let shared = self.data.shared.get();
        let server_actions = match self.data.actions.get() {
            Some(n) => n,
            _ => {
                log::error!("Server actions not set");
                return;
            }
        };
        let actions = server_actions & shared.receiver_actions.get();
        let action = if actions.contains(shared.receiver_preferred_action.get()) {
            shared.receiver_preferred_action.get()
        } else if actions != 0 {
            1 << actions.trailing_zeros()
        } else {
            0
        };
        if shared.selected_action.replace(action) != action {
            for (_, offer) in &self.data.offers {
                offer.send_action(action);
                // offer.client.flush();
            }
            self.send_action(action);
            // self.data.client.flush();
        }
    }

    pub fn for_each_data_offer<C: FnMut(&WlDataOffer)>(&self, mut f: C) {
        for (_, offer) in &self.data.offers {
            f(&offer);
        }
    }

    pub fn can_drop(&self) -> bool {
        let shared = self.data.shared.get();
        shared.selected_action.get() != 0 && shared.state.get().contains(OFFER_STATE_ACCEPTED)
    }

    pub fn on_drop(&self, seat: &Rc<WlSeatGlobal>) {
        self.data.state.or_assign(SOURCE_STATE_DROPPED);
        if let Some(drag) = self.toplevel_drag.take() {
            drag.finish_drag(seat);
        }
        self.send_dnd_drop_performed();
        let shared = self.data.shared.get();
        shared.state.or_assign(OFFER_STATE_DROPPED);
    }

    pub fn send_cancelled(self: &Rc<Self>, seat: &Rc<WlSeatGlobal>) {
        if self.data.is_xwm {
            self.data
                .client
                .state
                .xwayland
                .queue
                .push(XWaylandEvent::ClipboardCancelSource(self.clone()));
        } else {
            self.data.state.or_assign(SOURCE_STATE_CANCELLED);
            if let Some(drag) = self.toplevel_drag.take() {
                drag.finish_drag(seat);
            }
            self.data.client.event(Cancelled { self_id: self.id })
        }
    }

    pub fn send_send(self: &Rc<Self>, mime_type: &str, fd: Rc<OwnedFd>) {
        if self.data.is_xwm {
            self.data
                .client
                .state
                .xwayland
                .queue
                .push(XWaylandEvent::ClipboardSendSource(
                    self.clone(),
                    mime_type.to_string(),
                    fd,
                ));
        } else {
            self.data.client.event(Send {
                self_id: self.id,
                mime_type,
                fd,
            })
        }
    }

    pub fn send_target(&self, mime_type: Option<&str>) {
        if !self.data.is_xwm {
            self.data.client.event(Target {
                self_id: self.id,
                mime_type,
            })
        }
    }

    pub fn send_dnd_finished(&self) {
        if !self.data.is_xwm {
            self.data.client.event(DndFinished { self_id: self.id })
        }
    }

    pub fn send_action(&self, dnd_action: u32) {
        if !self.data.is_xwm {
            self.data.client.event(Action {
                self_id: self.id,
                dnd_action,
            })
        }
    }

    pub fn send_dnd_drop_performed(&self) {
        if !self.data.is_xwm {
            self.data
                .client
                .event(DndDropPerformed { self_id: self.id })
        }
    }

    fn offer(&self, parser: MsgParser<'_, '_>) -> Result<(), WlDataSourceError> {
        let req: Offer = self.data.client.parse(self, parser)?;
        add_data_source_mime_type::<ClipboardIpc>(self, req.mime_type);
        Ok(())
    }

    fn destroy(&self, parser: MsgParser<'_, '_>) -> Result<(), WlDataSourceError> {
        let _req: Destroy = self.data.client.parse(self, parser)?;
        destroy_data_source::<ClipboardIpc>(self);
        self.data.client.remove_obj(self)?;
        Ok(())
    }

    fn set_actions(&self, parser: MsgParser<'_, '_>) -> Result<(), WlDataSourceError> {
        let req: SetActions = self.data.client.parse(self, parser)?;
        if self.data.actions.is_some() {
            return Err(WlDataSourceError::AlreadySet);
        }
        if req.dnd_actions & !DND_ALL != 0 {
            return Err(WlDataSourceError::InvalidActions);
        }
        self.data.actions.set(Some(req.dnd_actions));
        Ok(())
    }
}

object_base! {
    self = WlDataSource;

    OFFER => offer,
    DESTROY => destroy,
    SET_ACTIONS => set_actions if self.version >= 3,
}

impl Object for WlDataSource {
    fn break_loops(&self) {
        break_source_loops::<ClipboardIpc>(self);
        self.toplevel_drag.take();
    }
}

dedicated_add_obj!(WlDataSource, WlDataSourceId, wl_data_source);

#[derive(Debug, Error)]
pub enum WlDataSourceError {
    #[error("Parsing failed")]
    MsgParserError(#[source] Box<MsgParserError>),
    #[error(transparent)]
    ClientError(Box<ClientError>),
    #[error("The set of actions is invalid")]
    InvalidActions,
    #[error("The actions have already been set")]
    AlreadySet,
}
efrom!(WlDataSourceError, ClientError);
efrom!(WlDataSourceError, MsgParserError);
