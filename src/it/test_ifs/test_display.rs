use {
    crate::{
        it::{
            test_error::TestError,
            test_object::{Deleted, TestObject},
            test_transport::TestTransport,
            testrun::ParseFull,
        },
        object::ObjectId,
        utils::buffd::MsgParser,
        wire::{wl_display::*, WlDisplayId},
    },
    std::rc::Rc,
};

pub struct TestDisplay {
    pub tran: Rc<TestTransport>,
    pub id: WlDisplayId,
    pub deleted: Deleted,
}

impl TestDisplay {
    fn handle_error(&self, parser: MsgParser<'_, '_>) -> Result<(), TestError> {
        let ev = Error::parse_full(parser)?;
        let msg = format!("Compositor sent an error: {}", ev.message);
        self.tran.error(&msg);
        self.tran.kill();
        Ok(())
    }

    fn handle_delete_id(&self, parser: MsgParser<'_, '_>) -> Result<(), TestError> {
        let ev = DeleteId::parse_full(parser)?;
        match self.tran.objects.remove(&ObjectId::from_raw(ev.id)) {
            None => {
                bail!(
                    "Compositor sent delete_id for object {} which does not exist",
                    ev.id
                );
            }
            Some(obj) => {
                obj.deleted().set();
                obj.on_remove(&self.tran);
                self.tran.obj_ids.borrow_mut().release(ev.id);
            }
        }
        Ok(())
    }
}

test_object! {
    TestDisplay, WlDisplay;

    ERROR => handle_error,
    DELETE_ID => handle_delete_id,
}

impl TestObject for TestDisplay {}
