use std::sync::Arc;

use editor::Editor;
use file_icons::FileIcons;
use gpui::{
    App, Context, DragMoveEvent, Empty, Entity, EventEmitter, FocusHandle, Focusable, IntoElement,
    ParentElement, Point, Render, RenderImage, ScrollWheelEvent, Styled, Subscription, SvgSize,
    Task, WeakEntity, Window, div, img,
};
use language::{Buffer, BufferEvent};
use ui::prelude::*;
use workspace::item::Item;
use workspace::{Pane, Workspace};

use crate::{OpenFollowingPreview, OpenPreview, OpenPreviewToTheSide};

pub struct SvgPreviewView {
    focus_handle: FocusHandle,
    buffer: Option<Entity<Buffer>>,
    current_svg: Option<Arc<RenderImage>>,
    error: Option<SharedString>,
    scale_factor: f32,
    drag_start: Point<Pixels>,
    image_offset: Point<Pixels>,
    _refresh: Task<()>,
    _buffer_subscription: Option<Subscription>,
    _workspace_subscription: Option<Subscription>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SvgPreviewMode {
    /// The preview will always show the contents of the provided editor.
    Default,
    /// The preview will "follow" the last active editor of an SVG file.
    Follow,
}

impl SvgPreviewView {
    pub fn new(
        mode: SvgPreviewMode,
        active_editor: Entity<Editor>,
        workspace_handle: WeakEntity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let workspace_subscription = if mode == SvgPreviewMode::Follow
                && let Some(workspace) = workspace_handle.upgrade()
            {
                Some(Self::subscribe_to_workspace(workspace, window, cx))
            } else {
                None
            };

            let buffer = active_editor
                .read(cx)
                .buffer()
                .clone()
                .read_with(cx, |buffer, _cx| buffer.as_singleton());

            let subscription = buffer
                .as_ref()
                .map(|buffer| Self::create_buffer_subscription(buffer, window, cx));

            let mut this = Self {
                focus_handle: cx.focus_handle(),
                buffer,
                error: None,
                current_svg: None,
                scale_factor: 1.0,
                drag_start: Default::default(),
                image_offset: Default::default(),
                _buffer_subscription: subscription,
                _workspace_subscription: workspace_subscription,
                _refresh: Task::ready(()),
            };
            this.render_image(window, cx);

            this
        })
    }

    fn subscribe_to_workspace(
        workspace: Entity<Workspace>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            &workspace,
            window,
            move |this: &mut SvgPreviewView, workspace, event: &workspace::Event, window, cx| {
                if let workspace::Event::ActiveItemChanged = event {
                    let workspace = workspace.read(cx);
                    if let Some(active_item) = workspace.active_item(cx)
                        && let Some(editor) = active_item.downcast::<Editor>()
                        && Self::is_svg_file(&editor, cx)
                    {
                        let Some(buffer) = editor.read(cx).buffer().read(cx).as_singleton() else {
                            return;
                        };
                        if this.buffer.as_ref() != Some(&buffer) {
                            this._buffer_subscription =
                                Some(Self::create_buffer_subscription(&buffer, window, cx));
                            this.buffer = Some(buffer);
                            this.render_image(window, cx);
                            cx.notify();
                        }
                    }
                }
            },
        )
    }

    fn render_image(&mut self, window: &Window, cx: &mut Context<Self>) {
        let Some(buffer) = self.buffer.as_ref() else {
            return;
        };

        let renderer = cx.svg_renderer();
        let content = buffer.read(cx).snapshot();
        let scale_factor = self.scale_factor;
        let background_task = cx.background_spawn(async move {
            renderer.render_single_frame(content.text().as_bytes(), scale_factor, true)
        });
        self._refresh = cx.spawn_in(window, async move |this, cx| {
            let result = background_task.await;

            this.update_in(cx, |view, window, cx| match result {
                Ok(image) => {
                    if let Some(image) = view.current_svg.take() {
                        window.drop_image(image).ok();
                    }
                    view.current_svg = Some(image);
                    view.error = None;
                    cx.notify();
                }
                Err(e) => view.error = Some(format!("{}", e).into()),
            })
            .ok();
        });
    }

    fn find_existing_preview_item_idx(
        pane: &Pane,
        editor: &Entity<Editor>,
        cx: &App,
    ) -> Option<usize> {
        let buffer_id = editor.read(cx).buffer().entity_id();
        pane.items_of_type::<SvgPreviewView>()
            .find(|view| {
                view.read(cx)
                    .buffer
                    .as_ref()
                    .is_some_and(|buffer| buffer.entity_id() == buffer_id)
            })
            .and_then(|view| pane.index_for_item(&view))
    }

    pub fn resolve_active_item_as_svg_editor(
        workspace: &Workspace,
        cx: &mut Context<Workspace>,
    ) -> Option<Entity<Editor>> {
        workspace
            .active_item(cx)?
            .act_as::<Editor>(cx)
            .filter(|editor| Self::is_svg_file(&editor, cx))
    }

    fn create_svg_view(
        mode: SvgPreviewMode,
        workspace: &mut Workspace,
        editor: Entity<Editor>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<SvgPreviewView> {
        let workspace_handle = workspace.weak_handle();
        SvgPreviewView::new(mode, editor, workspace_handle, window, cx)
    }

    fn create_buffer_subscription(
        buffer: &Entity<Buffer>,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(
            buffer,
            window,
            move |this, _buffer, event: &BufferEvent, window, cx| match event {
                BufferEvent::Edited | BufferEvent::Saved => {
                    this.render_image(window, cx);
                }
                _ => {}
            },
        )
    }

    pub fn is_svg_file(editor: &Entity<Editor>, cx: &App) -> bool {
        let buffer = editor.read(cx).buffer().read(cx);
        if let Some(buffer) = buffer.as_singleton()
            && let Some(file) = buffer.read(cx).file()
        {
            return file
                .path()
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| ext.eq_ignore_ascii_case("svg"))
                .unwrap_or(false);
        }
        false
    }

    pub fn register(workspace: &mut Workspace, _window: &mut Window, _cx: &mut Context<Workspace>) {
        workspace.register_action(move |workspace, _: &OpenPreview, window, cx| {
            if let Some(editor) = Self::resolve_active_item_as_svg_editor(workspace, cx)
                && Self::is_svg_file(&editor, cx)
            {
                let view = Self::create_svg_view(
                    SvgPreviewMode::Default,
                    workspace,
                    editor.clone(),
                    window,
                    cx,
                );
                workspace.active_pane().update(cx, |pane, cx| {
                    if let Some(existing_view_idx) =
                        Self::find_existing_preview_item_idx(pane, &editor, cx)
                    {
                        pane.activate_item(existing_view_idx, true, true, window, cx);
                    } else {
                        pane.add_item(Box::new(view), true, true, None, window, cx)
                    }
                });
                cx.notify();
            }
        });

        workspace.register_action(move |workspace, _: &OpenPreviewToTheSide, window, cx| {
            if let Some(editor) = Self::resolve_active_item_as_svg_editor(workspace, cx)
                && Self::is_svg_file(&editor, cx)
            {
                let editor_clone = editor.clone();
                let view = Self::create_svg_view(
                    SvgPreviewMode::Default,
                    workspace,
                    editor_clone,
                    window,
                    cx,
                );
                let pane = workspace
                    .find_pane_in_direction(workspace::SplitDirection::Right, cx)
                    .unwrap_or_else(|| {
                        workspace.split_pane(
                            workspace.active_pane().clone(),
                            workspace::SplitDirection::Right,
                            window,
                            cx,
                        )
                    });
                pane.update(cx, |pane, cx| {
                    if let Some(existing_view_idx) =
                        Self::find_existing_preview_item_idx(pane, &editor, cx)
                    {
                        pane.activate_item(existing_view_idx, true, true, window, cx);
                    } else {
                        pane.add_item(Box::new(view), false, false, None, window, cx)
                    }
                });
                cx.notify();
            }
        });

        workspace.register_action(move |workspace, _: &OpenFollowingPreview, window, cx| {
            if let Some(editor) = Self::resolve_active_item_as_svg_editor(workspace, cx)
                && Self::is_svg_file(&editor, cx)
            {
                let view =
                    Self::create_svg_view(SvgPreviewMode::Follow, workspace, editor, window, cx);
                workspace.active_pane().update(cx, |pane, cx| {
                    pane.add_item(Box::new(view), true, true, None, window, cx)
                });
                cx.notify();
            }
        });
    }
}

impl Render for SvgPreviewView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        struct DragStart {
            initial_offset: Point<Pixels>,
        }

        v_flex()
            .id("SvgPreview")
            .key_context("SvgPreview")
            .track_focus(&self.focus_handle(cx))
            .size_full()
            .bg(cx.theme().colors().editor_background)
            .flex()
            .justify_center()
            .items_center()
            .map(|this| {
                if let Some(content) = self.current_svg.clone() {
                    this.on_drag(
                        DragStart {
                            initial_offset: self.image_offset,
                        },
                        {
                            let this = cx.weak_entity();
                            move |_start, position, _, cx| {
                                this.update(cx, |this, _cx| {
                                    this.drag_start = position;
                                })
                                .ok();

                                cx.new(|_| Empty)
                            }
                        },
                    )
                    .on_drag_move(cx.listener(
                        |this, drag_move: &DragMoveEvent<DragStart>, _, cx| {
                            let drag_start = drag_move.drag(cx);
                            this.image_offset = drag_start.initial_offset
                                + drag_move.event.position
                                - drag_move.bounds.origin
                                - this.drag_start;
                            cx.notify();
                        },
                    ))
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                        let delta = event.delta.pixel_delta(px(1.)).y.0;
                        if delta.abs() != 0. {
                            this.scale_factor = (this.scale_factor + delta).clamp(0.25, 20.);
                            dbg!(this.scale_factor);
                            this.render_image(window, cx);
                        }
                    }))
                    .child(
                        img(content)
                            .object_fit(gpui::ObjectFit::None)
                            .absolute()
                            .left(self.image_offset.x)
                            .top(self.image_offset.y)
                            .max_w_full()
                            .max_h_full()
                            .with_fallback(|| {
                                h_flex()
                                    .p_4()
                                    .gap_2()
                                    .child(Icon::new(IconName::Warning))
                                    .child("Failed to load SVG file")
                                    .into_any_element()
                            }),
                    )
                } else {
                    this.child(div().p_4().child("No SVG file selected").into_any_element())
                }
            })
    }
}

impl Focusable for SvgPreviewView {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<()> for SvgPreviewView {}

impl Item for SvgPreviewView {
    type Event = ();

    fn tab_icon(&self, _window: &Window, cx: &App) -> Option<Icon> {
        self.buffer
            .as_ref()
            .and_then(|buffer| buffer.read(cx).file())
            .and_then(|file| FileIcons::get_icon(file.path(), cx))
            .map(Icon::from_path)
            .or_else(|| Some(Icon::new(IconName::Image)))
    }

    fn tab_content_text(&self, _detail: usize, cx: &App) -> SharedString {
        self.buffer
            .as_ref()
            .and_then(|svg_path| svg_path.read(cx).file())
            .map(|name| format!("Preview {}", name.file_name(cx).display()).into())
            .unwrap_or_else(|| "SVG Preview".into())
    }

    fn telemetry_event_text(&self) -> Option<&'static str> {
        Some("svg preview: open")
    }

    fn to_item_events(_event: &Self::Event, _f: impl FnMut(workspace::item::ItemEvent)) {}
}
