use {
    crate::{
        utils::{
            buffd::{MsgParser, MsgParserError},
            clonecell::CloneCell,
        },
        wire::{jay_workspace_watcher::*, JayWorkspaceWatcherId},
        wl_usr::{usr_ifs::usr_jay_workspace::UsrJayWorkspace, usr_object::UsrObject, UsrCon},
    },
    std::{ops::Deref, rc::Rc},
};

pub struct UsrJayWorkspaceWatcher {
    pub id: JayWorkspaceWatcherId,
    pub con: Rc<UsrCon>,
    pub owner: CloneCell<Option<Rc<dyn UsrJayWorkspaceWatcherOwner>>>,
}

pub trait UsrJayWorkspaceWatcherOwner {
    fn new(self: Rc<Self>, ev: Rc<UsrJayWorkspace>, linear_id: u32) {
        let _ = linear_id;
        ev.con.remove_obj(ev.deref());
    }
}

impl UsrJayWorkspaceWatcher {
    fn new(&self, parser: MsgParser<'_, '_>) -> Result<(), MsgParserError> {
        let ev: New = self.con.parse(self, parser)?;
        let jw = Rc::new(UsrJayWorkspace {
            id: ev.id,
            con: self.con.clone(),
            owner: Default::default(),
        });
        self.con.add_object(jw.clone());
        if let Some(owner) = self.owner.get() {
            owner.new(jw, ev.linear_id);
        } else {
            self.con.remove_obj(jw.deref());
        }
        Ok(())
    }
}

usr_object_base! {
    UsrJayWorkspaceWatcher, JayWorkspaceWatcher;

    NEW => new,
}

impl UsrObject for UsrJayWorkspaceWatcher {
    fn destroy(&self) {
        self.con.request(Destroy { self_id: self.id });
    }

    fn break_loops(&self) {
        self.owner.set(None);
    }
}
