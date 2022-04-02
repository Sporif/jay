use crate::backend::Output;
use crate::ifs::wl_output::WlOutputGlobal;
use crate::rect::Rect;
use crate::state::State;
use crate::tree::{OutputNode, OutputRenderData, WorkspaceNode};
use crate::utils::asyncevent::AsyncEvent;
use crate::utils::clonecell::CloneCell;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

pub struct OutputHandler {
    pub state: Rc<State>,
    pub output: Rc<dyn Output>,
}

impl OutputHandler {
    pub async fn handle(self) {
        let ae = Rc::new(AsyncEvent::default());
        {
            let ae = ae.clone();
            self.output.on_change(Rc::new(move || ae.trigger()));
        }
        let name = self.state.globals.name();
        let global = Rc::new(WlOutputGlobal::new(name, self.output.clone()));
        let x1 = self.state.root.outputs.lock().values().map(|o| o.position.get().x2()).max().unwrap_or(0);
        let on = Rc::new(OutputNode {
            id: self.state.node_ids.next(),
            workspaces: Default::default(),
            position: Cell::new(Rect::new_empty(x1, 0)),
            workspace: CloneCell::new(None),
            seat_state: Default::default(),
            global: global.clone(),
            layers: Default::default(),
            render_data: RefCell::new(OutputRenderData {
                active_workspace: Rect::new_empty(0, 0),
                inactive_workspaces: Default::default(),
                titles: Default::default(),
            }),
            state: self.state.clone(),
            is_dummy: false,
        });
        global.node.set(Some(on.clone()));
        let name = 'name: {
            for i in 1.. {
                let name = i.to_string();
                if !self.state.workspaces.contains(&name) {
                    break 'name name;
                }
            }
            unreachable!();
        };
        let workspace = Rc::new(WorkspaceNode {
            id: self.state.node_ids.next(),
            output: CloneCell::new(on.clone()),
            position: Default::default(),
            container: Default::default(),
            stacked: Default::default(),
            seat_state: Default::default(),
            name: name.clone(),
            output_link: Default::default(),
        });
        self.state.workspaces.set(name, workspace.clone());
        workspace
            .output_link
            .set(Some(on.workspaces.add_last(workspace.clone())));
        on.show_workspace(&workspace);
        on.update_render_data();
        self.state.root.outputs.set(self.output.id(), on.clone());
        self.state.add_global(&global);
        self.state.outputs.set(self.output.id(), global.clone());
        let mut width = 0;
        let mut height = 0;
        loop {
            if self.output.removed() {
                break;
            }
            let new_width = self.output.width();
            let new_height = self.output.height();
            if new_width != width || new_height != height {
                width = new_width;
                height = new_height;
                on.change_size(new_width, new_height);
            }
            global.update_properties();
            ae.triggered().await;
        }
        global.node.set(None);
        self.state.outputs.remove(&self.output.id());
        let _ = self.state.remove_global(&*global);
        self.state
            .output_handlers
            .borrow_mut()
            .remove(&self.output.id());
        self.state.root.outputs.remove(&self.output.id());
    }
}
