//! The main window: a mixer of input strips and output buses, a device list
//! for the local network, and settings.

use crate::theme::{Theme, ui_font, with_alpha};
use crate::widgets::*;
use crossbeam_channel::Receiver;
use gpui::{
    AnyElement, ClickEvent, Context, Div, FontWeight, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, ScrollDelta, ScrollWheelEvent, SharedString, Stateful, Window, WindowAppearance, anchored,
    deferred, div, point, prelude::*, px, relative,
};
use relay_ipc::Client;
use relay_proto::*;
use std::collections::HashMap;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq)]
pub enum Conn {
    Starting,
    Connected,
    Lost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Target {
    Strip(StripId),
    Bus(BusId),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Param {
    Gain,
    EqBand(usize),
    EqQ(usize),
    GateThreshold,
    CompThreshold,
    CompRatio,
    CompMakeup,
    LimiterCeiling,
}

pub struct Drag {
    pub target: Target,
    pub param: Param,
    pub start: Point<Pixels>,
    pub start_a: f32,
    pub start_b: f32,
    /// Pixel size used to scale movement (track height / plot size).
    pub scale: (f32, f32),
}

#[derive(Clone, Debug)]
pub enum MenuKind {
    StripSource(StripId),
    BusSink(BusId),
    AddStrip,
    AddBus,
}

pub struct Menu {
    pub kind: MenuKind,
    pub pos: Point<Pixels>,
}

#[derive(Clone, Copy, Default)]
struct Disp {
    l: f32,
    r: f32,
    red: f32,
}

#[derive(PartialEq)]
enum Page {
    Mixer,
    Settings,
}

pub struct RelayApp {
    pub theme: Theme,
    conn: Conn,
    client: Option<Client>,
    rx: Option<Receiver<ServerMsg>>,
    pub state: Option<EngineState>,
    meters: HashMap<Target, Disp>,
    meter_time: Instant,
    pub drag: Option<Drag>,
    menu: Option<Menu>,
    page: Page,
    pub fx: Option<Target>,
    expanded_peer: Option<PeerId>,
    notice: Option<(SharedString, bool, Instant)>,
    last_try: Instant,
    launched_at: Instant,
    appearance: Option<WindowAppearance>,
    quit_requested: bool,
}

impl RelayApp {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(Duration::from_millis(16)).await;
                if this.update(cx, |app, cx| app.poll(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
        let appearance = window.appearance();
        let mut app = Self {
            theme: theme_for(appearance),
            conn: Conn::Starting,
            client: None,
            rx: None,
            state: None,
            meters: HashMap::new(),
            meter_time: Instant::now(),
            drag: None,
            menu: None,
            page: Page::Mixer,
            fx: None,
            expanded_peer: None,
            notice: None,
            last_try: Instant::now() - Duration::from_secs(5),
            launched_at: Instant::now(),
            appearance: Some(appearance),
            quit_requested: false,
        };
        app.try_connect();
        app
    }

    fn try_connect(&mut self) {
        self.last_try = Instant::now();
        match Client::connect("relay-app") {
            Ok((client, rx)) => {
                let _ = client.send(&ClientMsg::Meters { on: true });
                self.client = Some(client);
                self.rx = Some(rx);
                self.conn = Conn::Connected;
            }
            Err(_) => {
                if self.conn == Conn::Connected {
                    self.conn = Conn::Lost;
                }
                if self.launched_at.elapsed() > Duration::from_secs(8) && self.conn == Conn::Starting {
                    self.conn = Conn::Lost;
                }
            }
        }
    }

    fn poll(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        if self.client.is_none() && self.last_try.elapsed() > Duration::from_millis(500) {
            self.try_connect();
            changed = true;
        }
        if let Some(rx) = self.rx.clone() {
            loop {
                match rx.try_recv() {
                    Ok(msg) => {
                        changed = true;
                        self.on_msg(msg);
                    }
                    Err(crossbeam_channel::TryRecvError::Empty) => break,
                    Err(crossbeam_channel::TryRecvError::Disconnected) => {
                        self.client = None;
                        self.rx = None;
                        self.conn = Conn::Lost;
                        changed = true;
                        break;
                    }
                }
            }
        }
        // Meter ballistics: fall at ~24 dB/s.
        let dt = self.meter_time.elapsed().as_secs_f32();
        self.meter_time = Instant::now();
        let fall = 10f32.powf(-24.0 * dt / 20.0);
        for d in self.meters.values_mut() {
            if d.l > 0.0005 || d.r > 0.0005 {
                changed = true;
            }
            d.l *= fall;
            d.r *= fall;
        }
        if let Some((_, _, t)) = &self.notice
            && t.elapsed() > Duration::from_secs(5)
        {
            self.notice = None;
            changed = true;
        }
        if self.quit_requested {
            cx.quit();
            return;
        }
        if changed {
            cx.notify();
        }
    }

    fn on_msg(&mut self, msg: ServerMsg) {
        match msg {
            ServerMsg::Hello { engine_version, .. } => {
                if engine_version != crate::VERSION && crate::engine_proc::relaunch_updated(&engine_version) {
                    self.quit_requested = true;
                }
            }
            ServerMsg::State(s) => {
                // Keep local optimistic values while dragging that control.
                let mut s = *s;
                if let (Some(drag), Some(old)) = (&self.drag, &self.state) {
                    match drag.target {
                        Target::Strip(id) => {
                            if let (Some(n), Some(o)) = (
                                s.mixer.strips.iter_mut().find(|x| x.id == id),
                                old.mixer.strips.iter().find(|x| x.id == id),
                            ) {
                                n.gain_db = o.gain_db;
                                n.effects = o.effects.clone();
                            }
                        }
                        Target::Bus(id) => {
                            if let (Some(n), Some(o)) = (
                                s.mixer.buses.iter_mut().find(|x| x.id == id),
                                old.mixer.buses.iter().find(|x| x.id == id),
                            ) {
                                n.gain_db = o.gain_db;
                                n.effects = o.effects.clone();
                            }
                        }
                    }
                }
                self.state = Some(s);
            }
            ServerMsg::Meters(m) => {
                for (id, l) in m.strips {
                    let d = self.meters.entry(Target::Strip(id)).or_default();
                    d.l = d.l.max(l.peak_l);
                    d.r = d.r.max(l.peak_r);
                    d.red = l.reduction_db;
                }
                for (id, l) in m.buses {
                    let d = self.meters.entry(Target::Bus(id)).or_default();
                    d.l = d.l.max(l.peak_l);
                    d.r = d.r.max(l.peak_r);
                    d.red = l.reduction_db;
                }
            }
            ServerMsg::Notice { text, error } => self.notice = Some((text.into(), error, Instant::now())),
        }
    }

    pub fn cmd(&mut self, c: Command) {
        if let Some(client) = &self.client
            && client.send(&ClientMsg::Command(c)).is_err()
        {
            self.client = None;
            self.rx = None;
            self.conn = Conn::Lost;
        }
    }

    pub fn strip(&self, id: StripId) -> Option<&Strip> {
        self.state.as_ref()?.mixer.strips.iter().find(|s| s.id == id)
    }

    pub fn bus(&self, id: BusId) -> Option<&Bus> {
        self.state.as_ref()?.mixer.buses.iter().find(|b| b.id == id)
    }

    pub fn effects_of(&self, t: Target) -> Option<Effects> {
        match t {
            Target::Strip(id) => self.strip(id).map(|s| s.effects.clone()),
            Target::Bus(id) => self.bus(id).map(|b| b.effects.clone()),
        }
    }

    pub fn set_effects(&mut self, t: Target, fx: Effects) {
        if let Some(s) = self.state.as_mut() {
            match t {
                Target::Strip(id) => {
                    if let Some(x) = s.mixer.strips.iter_mut().find(|x| x.id == id) {
                        x.effects = fx.clone();
                    }
                    self.cmd(Command::SetStripEffects { id, effects: fx });
                }
                Target::Bus(id) => {
                    if let Some(x) = s.mixer.buses.iter_mut().find(|x| x.id == id) {
                        x.effects = fx.clone();
                    }
                    self.cmd(Command::SetBusEffects { id, effects: fx });
                }
            }
        }
    }

    fn gain_of(&self, t: Target) -> f32 {
        match t {
            Target::Strip(id) => self.strip(id).map(|s| s.gain_db),
            Target::Bus(id) => self.bus(id).map(|b| b.gain_db),
        }
        .unwrap_or(0.0)
    }

    pub fn set_gain(&mut self, t: Target, db: f32) {
        let db = (db * 10.0).round() / 10.0;
        if let Some(s) = self.state.as_mut() {
            match t {
                Target::Strip(id) => {
                    if let Some(x) = s.mixer.strips.iter_mut().find(|x| x.id == id) {
                        x.gain_db = db;
                    }
                    self.cmd(Command::SetStripGain { id, gain_db: db });
                }
                Target::Bus(id) => {
                    if let Some(x) = s.mixer.buses.iter_mut().find(|x| x.id == id) {
                        x.gain_db = db;
                    }
                    self.cmd(Command::SetBusGain { id, gain_db: db });
                }
            }
        }
    }

    // ------------------------------------------------------------ drag handling

    fn on_root_move(&mut self, ev: &MouseMoveEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let Some(drag) = &self.drag else { return };
        if ev.pressed_button != Some(MouseButton::Left) {
            self.drag = None;
            cx.notify();
            return;
        }
        let fine = if ev.modifiers.shift { 0.2 } else { 1.0 };
        let dx = f32::from(ev.position.x - drag.start.x) * fine;
        let dy = f32::from(ev.position.y - drag.start.y) * fine;
        let (target, param, a, b, scale) = (drag.target, drag.param, drag.start_a, drag.start_b, drag.scale);
        match param {
            Param::Gain => {
                let pos = a - dy / scale.1;
                self.set_gain(target, pos_to_db(pos));
            }
            other => crate::fx::drag_param(self, target, other, a, b, dx, dy, scale),
        }
        cx.notify();
    }

    fn on_root_up(&mut self, _: &MouseUpEvent, _w: &mut Window, cx: &mut Context<Self>) {
        if self.drag.take().is_some() {
            cx.notify();
        }
    }

    pub fn start_drag(
        &mut self,
        target: Target,
        param: Param,
        start: Point<Pixels>,
        a: f32,
        b: f32,
        scale: (f32, f32),
    ) {
        self.drag = Some(Drag { target, param, start, start_a: a, start_b: b, scale });
    }

    // ------------------------------------------------------------ rendering

    fn header(&mut self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let (color, label): (gpui::Rgba, SharedString) = match &self.conn {
            Conn::Starting => (t.warn, "Starting".into()),
            Conn::Connected => (t.good, "Running".into()),
            Conn::Lost => (t.bad, "Engine stopped".into()),
        };
        let left_pad = if cfg!(target_os = "macos") { px(84.0) } else { px(16.0) };
        div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(48.0))
            .pl(left_pad)
            .pr(px(12.0))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.0))
                    .child(div().text_size(px(15.0)).font_weight(FontWeight::BOLD).text_color(t.text).child("Relay"))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .text_size(px(12.0))
                            .text_color(t.muted)
                            .child(dot(color))
                            .child(label),
                    )
                    .when(self.conn == Conn::Lost, |d| {
                        d.child(button("restart-engine", "Restart", &t).on_click(cx.listener(|this, _, _, cx| {
                            crate::engine_proc::launch();
                            this.launched_at = Instant::now();
                            this.conn = Conn::Starting;
                            cx.notify();
                        })))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.0))
                    .child(chip("page-mixer", "Mixer", self.page == Page::Mixer, &t).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.page = Page::Mixer;
                            cx.notify();
                        },
                    )))
                    .child(chip("page-settings", "Settings", self.page == Page::Settings, &t).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.page = Page::Settings;
                            cx.notify();
                        },
                    ))),
            )
    }

    fn banner(&mut self, cx: &mut Context<Self>) -> Option<Div> {
        let t = self.theme;
        let s = self.state.as_ref()?;
        let row = |color: gpui::Rgba| {
            div()
                .flex()
                .items_center()
                .gap(px(10.0))
                .mx(px(16.0))
                .mt(px(12.0))
                .px(px(12.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(with_alpha(color, 0.12))
                .border_1()
                .border_color(with_alpha(color, 0.35))
                .text_size(px(12.0))
                .text_color(t.text)
        };
        match &s.virtual_audio.state {
            VirtualAudioState::NeedsInstall => Some(
                row(t.warn)
                    .child(dot(t.warn))
                    .child(div().flex_1().child("Relay's audio devices need a one-time install."))
                    .child(
                        primary_button("install-va", "Install", &t)
                            .on_click(cx.listener(|this, _, _, _| this.cmd(Command::InstallVirtualAudio))),
                    ),
            ),
            VirtualAudioState::Installing => {
                Some(row(t.accent).child(dot(t.accent)).child("Installing Relay's audio devices…"))
            }
            VirtualAudioState::Error(e) => Some(
                row(t.bad).child(dot(t.bad)).child(div().flex_1().child(SharedString::from(e.clone()))).child(
                    button("retry-va", "Retry", &t)
                        .on_click(cx.listener(|this, _, _, _| this.cmd(Command::InstallVirtualAudio))),
                ),
            ),
            _ => None,
        }
    }

    fn port_status(&self, status: &PortStatus) -> Option<(gpui::Rgba, SharedString)> {
        let t = &self.theme;
        match status {
            PortStatus::Missing(m) => Some((t.warn, m.clone().into())),
            PortStatus::Error(m) => Some((t.bad, m.clone().into())),
            PortStatus::Starting => Some((t.muted, "Starting…".into())),
            _ => None,
        }
    }

    fn meter_and_fader(&mut self, target: Target, gain_db: f32, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let disp = self.meters.get(&target).copied().unwrap_or_default();
        let pos = db_to_pos(gain_db);
        let bar = |lvl: f32| {
            div().w(px(5.0)).h_full().rounded(px(2.0)).bg(t.track).relative().child(
                div()
                    .absolute()
                    .bottom_0()
                    .left_0()
                    .w_full()
                    .h(relative(level_pos(lvl)))
                    .rounded(px(2.0))
                    .bg(level_color(&t, lvl)),
            )
        };
        let id_base: SharedString = match target {
            Target::Strip(id) => format!("s{id}").into(),
            Target::Bus(id) => format!("b{id}").into(),
        };
        let track = div()
            .id((id_base.clone(), 0usize))
            .relative()
            .w(px(28.0))
            .h_full()
            .cursor_ns_resize()
            .child(div().absolute().left(px(12.0)).top_0().bottom_0().w(px(4.0)).rounded(px(2.0)).bg(t.track))
            .child(
                div()
                    .absolute()
                    .left(px(12.0))
                    .bottom_0()
                    .w(px(4.0))
                    .h(relative(pos))
                    .rounded(px(2.0))
                    .bg(with_alpha(t.accent, 0.55)),
            )
            .child(
                div()
                    .absolute()
                    .left(px(2.0))
                    .bottom(relative(pos))
                    .mb(px(-7.0))
                    .w(px(24.0))
                    .h(px(14.0))
                    .rounded(px(4.0))
                    .bg(t.text)
                    .border_1()
                    .border_color(t.border)
                    .shadow_sm(),
            )
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    if ev.click_count >= 2 {
                        this.set_gain(target, 0.0);
                    } else {
                        let g = this.gain_of(target);
                        this.start_drag(target, Param::Gain, ev.position, db_to_pos(g), 0.0, (1.0, 220.0));
                    }
                    cx.notify();
                }),
            )
            .on_scroll_wheel(cx.listener(move |this, ev: &ScrollWheelEvent, _, cx| {
                let dy = match ev.delta {
                    ScrollDelta::Lines(l) => l.y,
                    ScrollDelta::Pixels(p) => f32::from(p.y) / 20.0,
                };
                let g = this.gain_of(target);
                this.set_gain(target, (g + dy * 0.5).clamp(-60.0, 12.0));
                cx.notify();
            }));
        div()
            .flex()
            .flex_1()
            .min_h(px(80.0))
            .justify_center()
            .gap(px(6.0))
            .py(px(8.0))
            .child(div().flex().gap(px(2.0)).h_full().child(bar(disp.l)).child(bar(disp.r)))
            .child(track)
    }

    fn strip_card(&mut self, s: &Strip, buses: &[Bus], cx: &mut Context<Self>) -> Stateful<Div> {
        let t = self.theme;
        let id = s.id;
        let target = Target::Strip(id);
        let status = self.port_status(&s.status);
        let source_label: SharedString = source_label(&s.source).into();
        let fx_on = self.fx == Some(target);
        let fx_active = fx_active(&s.effects);
        let card = card(("strip", id as usize), &t, fx_on)
            .child(card_title(&s.name, &t))
            .child(
                div()
                    .id(("src", id as usize))
                    .text_size(px(11.0))
                    .text_color(t.muted)
                    .cursor_pointer()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .hover(|d| d.text_color(t.text))
                    .child(format!("{source_label} ▾"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.menu = Some(Menu { kind: MenuKind::StripSource(id), pos: ev.position });
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            )
            .child(status_line(status))
            .child(self.meter_and_fader(target, s.gain_db, cx))
            .child(div().text_size(px(11.0)).text_color(t.muted).flex().justify_center().child(fmt_db(s.gain_db)))
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .justify_center()
                    .child(chip(("mute", id as usize), "Mute", s.mute, &t).on_click(cx.listener(
                        move |this, _, _, cx| {
                            let m = this.strip(id).map(|s| !s.mute).unwrap_or(true);
                            if let Some(st) = this.state.as_mut()
                                && let Some(x) = st.mixer.strips.iter_mut().find(|x| x.id == id)
                            {
                                x.mute = m;
                            }
                            this.cmd(Command::SetStripMute { id, mute: m });
                            cx.notify();
                        },
                    )))
                    .child(chip(("fx", id as usize), "FX", fx_on || fx_active, &t).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.fx = if this.fx == Some(target) { None } else { Some(target) };
                            cx.notify();
                        },
                    ))),
            )
            .child(div().h(px(1.0)).bg(t.border).my(px(2.0)))
            .child(div().flex().flex_col().gap(px(3.0)).children(buses.iter().map(|b| {
                let on = s.routes.contains(&b.id);
                let bus_id = b.id;
                chip(("route", (id as usize) << 16 | b.id as usize), b.name.clone(), on, &t).on_click(cx.listener(
                    move |this, _, _, cx| {
                        let on = !this.strip(id).map(|s| s.routes.contains(&bus_id)).unwrap_or(false);
                        if let Some(st) = this.state.as_mut()
                            && let Some(x) = st.mixer.strips.iter_mut().find(|x| x.id == id)
                        {
                            x.routes.retain(|b| *b != bus_id);
                            if on {
                                x.routes.push(bus_id);
                            }
                        }
                        this.cmd(Command::SetRoute { strip: id, bus: bus_id, on });
                        cx.notify();
                    },
                ))
            })));
        card
    }

    fn bus_card(&mut self, b: &Bus, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = self.theme;
        let id = b.id;
        let target = Target::Bus(id);
        let status = self.port_status(&b.status);
        let sink_label: SharedString = self.sink_label(&b.sink).into();
        let fx_on = self.fx == Some(target);
        let fx_active = fx_active(&b.effects);
        card(("bus", id as usize), &t, fx_on)
            .child(card_title(&b.name, &t))
            .child(
                div()
                    .id(("sink", id as usize))
                    .text_size(px(11.0))
                    .text_color(t.muted)
                    .cursor_pointer()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .hover(|d| d.text_color(t.text))
                    .child(format!("{sink_label} ▾"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                            this.menu = Some(Menu { kind: MenuKind::BusSink(id), pos: ev.position });
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    ),
            )
            .child(status_line(status))
            .child(self.meter_and_fader(target, b.gain_db, cx))
            .child(div().text_size(px(11.0)).text_color(t.muted).flex().justify_center().child(fmt_db(b.gain_db)))
            .child(
                div()
                    .flex()
                    .gap(px(4.0))
                    .justify_center()
                    .child(chip(("bmute", id as usize), "Mute", b.mute, &t).on_click(cx.listener(
                        move |this, _, _, cx| {
                            let m = this.bus(id).map(|b| !b.mute).unwrap_or(true);
                            if let Some(st) = this.state.as_mut()
                                && let Some(x) = st.mixer.buses.iter_mut().find(|x| x.id == id)
                            {
                                x.mute = m;
                            }
                            this.cmd(Command::SetBusMute { id, mute: m });
                            cx.notify();
                        },
                    )))
                    .child(chip(("bfx", id as usize), "FX", fx_on || fx_active, &t).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.fx = if this.fx == Some(target) { None } else { Some(target) };
                            cx.notify();
                        },
                    ))),
            )
    }

    fn add_card(&mut self, id: &'static str, kind: MenuKind, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = self.theme;
        div()
            .id(id)
            .w(px(44.0))
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(10.0))
            .border_1()
            .border_dashed()
            .border_color(t.border)
            .text_color(t.faint)
            .text_size(px(18.0))
            .cursor_pointer()
            .hover(|d| d.text_color(t.text).border_color(t.muted))
            .child("+")
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, ev: &MouseDownEvent, _, cx| {
                    this.menu = Some(Menu { kind: kind.clone(), pos: ev.position });
                    cx.stop_propagation();
                    cx.notify();
                }),
            )
    }

    fn mixer(&mut self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let Some(state) = self.state.clone() else {
            return div().flex_1().flex().items_center().justify_center().text_color(t.muted).child(match self.conn {
                Conn::Lost => "Relay's engine isn't running.",
                _ => "Starting…",
            });
        };
        let strips: Vec<_> =
            state.mixer.strips.iter().map(|s| self.strip_card(s, &state.mixer.buses, cx).into_any_element()).collect();
        let buses: Vec<_> = state.mixer.buses.iter().map(|b| self.bus_card(b, cx).into_any_element()).collect();
        let add_in = self.add_card("add-strip", MenuKind::AddStrip, cx);
        let add_out = self.add_card("add-bus", MenuKind::AddBus, cx);
        div()
            .flex_1()
            .min_h(px(0.0))
            .flex()
            .flex_col()
            .child(
                div()
                    .id("mixer-scroll")
                    .flex_1()
                    .min_h(px(0.0))
                    .overflow_x_scroll()
                    .flex()
                    .gap(px(16.0))
                    .p(px(16.0))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(section_title("INPUTS", &t))
                            .child(div().flex().flex_1().gap(px(8.0)).children(strips).child(add_in)),
                    )
                    .child(div().w(px(1.0)).bg(t.border))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(8.0))
                            .child(section_title("OUTPUTS", &t))
                            .child(div().flex().flex_1().gap(px(8.0)).children(buses).child(add_out)),
                    ),
            )
            .when_some(self.fx, |d, target| d.child(crate::fx::panel(self, target, cx)))
    }

    fn peers(&mut self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let Some(state) = self.state.clone() else { return div() };
        let net = &state.network;
        let mut list = div().flex().flex_col().gap(px(6.0));
        if !net.enabled {
            list = list.child(muted("Network is off.", &t).text_size(px(12.0)));
        } else if let Some(p) = &net.problem {
            list = list.child(
                div()
                    .flex()
                    .gap(px(6.0))
                    .items_center()
                    .text_size(px(12.0))
                    .text_color(t.bad)
                    .child(dot(t.bad))
                    .child(SharedString::from(p.clone())),
            );
        }
        if net.enabled && net.firewall_blocked {
            list = list.child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .items_center()
                    .text_size(px(12.0))
                    .text_color(t.warn)
                    .child(dot(t.warn))
                    .child(div().flex_1().child("Windows Firewall is blocking Relay"))
                    .child(
                        button("fix-fw", "Fix", &t)
                            .on_click(cx.listener(|this, _, _, _| this.cmd(Command::FixFirewall))),
                    ),
            );
        }
        if net.enabled && net.peers.is_empty() && net.problem.is_none() {
            list = list.child(muted("Looking for devices…", &t).text_size(px(12.0)));
        }
        for p in &net.peers {
            list = list.child(self.peer_row(p, cx));
        }
        div()
            .w(px(280.0))
            .flex_shrink_0()
            .h_full()
            .border_l_1()
            .border_color(t.border)
            .bg(t.panel)
            .p(px(14.0))
            .flex()
            .flex_col()
            .gap(px(10.0))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(section_title("DEVICES", &t))
                    .child(muted(net.this_name.clone(), &t).text_size(px(11.0))),
            )
            .child(list)
    }

    fn peer_row(&mut self, p: &Peer, cx: &mut Context<Self>) -> Stateful<Div> {
        let t = self.theme;
        let (color, status): (gpui::Rgba, String) = match &p.state {
            LinkState::Available => (t.faint, "Available".into()),
            LinkState::Requesting => (t.warn, "Waiting for approval".into()),
            LinkState::Incoming => (t.accent, "Wants to connect".into()),
            LinkState::Connecting => (t.warn, "Connecting…".into()),
            LinkState::Live => (t.good, format!("Connected · {:.0} ms", p.stats.rtt_ms.max(1.0))),
            LinkState::Degraded(why) => (t.warn, why.clone()),
            LinkState::Lost(why) => (t.bad, why.clone()),
            LinkState::Refused(why) => (t.bad, why.clone()),
            LinkState::Offline => (t.faint, "Offline".into()),
        };
        let peer_id = p.id.clone();
        let expanded = self.expanded_peer.as_ref() == Some(&p.id);
        let platform = match p.platform {
            Platform::Windows => "Windows",
            Platform::Macos => "Mac",
            Platform::Linux => "Linux",
            Platform::Other => "",
        };
        let mut actions = div().flex().gap(px(6.0)).flex_wrap();
        let pid = p.id.clone();
        match &p.state {
            LinkState::Available | LinkState::Offline | LinkState::Lost(_) | LinkState::Refused(_)
                if !p.paired || !matches!(p.state, LinkState::Offline) =>
            {
                actions =
                    actions.child(primary_button(("connect", hash(&pid)), "Connect", &t).on_click(cx.listener({
                        let pid = pid.clone();
                        move |this, _, _, _| this.cmd(Command::Connect { peer: pid.clone() })
                    })));
            }
            LinkState::Incoming => {
                actions = actions
                    .child(primary_button(("allow", hash(&pid)), "Allow", &t).on_click(cx.listener({
                        let pid = pid.clone();
                        move |this, _, _, _| this.cmd(Command::AcceptPeer { peer: pid.clone() })
                    })))
                    .child(button(("deny", hash(&pid)), "Decline", &t).on_click(cx.listener({
                        let pid = pid.clone();
                        move |this, _, _, _| this.cmd(Command::RejectPeer { peer: pid.clone() })
                    })));
            }
            LinkState::Live | LinkState::Degraded(_) => {
                let sending = self
                    .state
                    .as_ref()
                    .map(|s| s.mixer.buses.iter().any(|b| matches!(&b.sink, Sink::Network { peer } if *peer == pid)))
                    .unwrap_or(false);
                if !sending {
                    actions = actions.child(button(("send", hash(&pid)), "Send audio", &t).on_click(cx.listener({
                        let pid = pid.clone();
                        move |this, _, _, _| this.cmd(Command::AddBus { sink: Sink::Network { peer: pid.clone() } })
                    })));
                }
                let video_sources = self.state.as_ref().map(|s| s.video.sources.clone()).unwrap_or_default();
                let already =
                    self.state.as_ref().map(|s| s.video.sending.iter().find(|v| v.peer == pid).map(|v| v.id)).flatten();
                match already {
                    Some(vid) => {
                        actions = actions.child(
                            button(("stopv", hash(&pid)), "Stop video", &t)
                                .on_click(cx.listener(move |this, _, _, _| this.cmd(Command::StopVideo { id: vid }))),
                        );
                    }
                    None => {
                        if let Some(screen) = video_sources.iter().find(|v| v.kind == VideoSourceKind::Screen) {
                            let key = screen.key.clone();
                            actions = actions.child(button(("sharev", hash(&pid)), "Share screen", &t).on_click(
                                cx.listener({
                                    let pid = pid.clone();
                                    move |this, _, _, _| {
                                        this.cmd(Command::StartVideo {
                                            source: key.clone(),
                                            peer: pid.clone(),
                                            with_audio: true,
                                        })
                                    }
                                }),
                            ));
                        }
                        if let Some(cam) = video_sources.iter().find(|v| v.kind == VideoSourceKind::Camera) {
                            let key = cam.key.clone();
                            actions = actions.child(button(("camv", hash(&pid)), "Share camera", &t).on_click(
                                cx.listener({
                                    let pid = pid.clone();
                                    move |this, _, _, _| {
                                        this.cmd(Command::StartVideo {
                                            source: key.clone(),
                                            peer: pid.clone(),
                                            with_audio: false,
                                        })
                                    }
                                }),
                            ));
                        }
                    }
                }
            }
            _ => {}
        }
        // Video going either way, plus streams that can be (re)opened.
        let mut media = div().flex().flex_col().gap(px(2.0)).text_size(px(11.0)).text_color(t.muted);
        if let Some(st) = &self.state {
            for v in st.video.sending.iter().filter(|v| v.peer == p.id) {
                let what = st
                    .video
                    .sources
                    .iter()
                    .find(|s| s.key == v.source)
                    .map(|s| s.name.clone())
                    .unwrap_or_else(|| "Video".into());
                let line = match &v.status {
                    PortStatus::Error(e) => format!("{what}: {e}"),
                    _ if v.fps > 0.0 => format!("Sending {what} · {}×{} · {:.0} fps", v.width, v.height, v.fps),
                    _ => format!("Sending {what}"),
                };
                media = media.child(div().child(line));
            }
            for inc in p.incoming.iter().filter(|s| s.kind == StreamKind::Video) {
                let open = st.video.receiving.iter().any(|r| r.peer == p.id && r.stream == inc.id);
                let label = if inc.name.is_empty() { "Video".to_string() } else { inc.name.clone() };
                if open {
                    media = media.child(div().child(format!("Showing {label}")));
                } else {
                    let (peer, stream) = (p.id.clone(), inc.id);
                    media = media.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.0))
                            .child(div().flex_1().child(format!("{label} available")))
                            .child(button(("watch", hash(&p.id) ^ inc.id as usize), "Watch", &t).on_click(
                                cx.listener(move |this, _, _, _| {
                                    this.cmd(Command::WatchVideo { peer: peer.clone(), stream })
                                }),
                            )),
                    );
                }
            }
        }
        let s = &p.stats;
        let details = div()
            .flex()
            .flex_col()
            .gap(px(2.0))
            .text_size(px(11.0))
            .text_color(t.muted)
            .child(format!("Round trip {:.1} ms · jitter {:.1} ms", s.rtt_ms, s.jitter_ms))
            .child(format!("Loss {:.1}% · buffer {:.0} ms · glitches {}", s.loss_pct, s.buffer_ms, s.glitches))
            .child(format!("Up {:.0} kb/s · down {:.0} kb/s", s.send_kbps, s.recv_kbps))
            .child(format!("{} · Relay {}", p.addresses.join(", "), p.version))
            .child(format!("ID {}", &p.id))
            .when(p.paired, |d| {
                d.child(
                    div()
                        .id(("forget", hash(&pid)))
                        .text_color(t.bad)
                        .cursor_pointer()
                        .child("Forget this device")
                        .on_click(cx.listener({
                            let pid = pid.clone();
                            move |this, _, _, _| this.cmd(Command::ForgetPeer { peer: pid.clone() })
                        })),
                )
            });
        div()
            .id(("peer", hash(&p.id)))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .p(px(10.0))
            .rounded(px(8.0))
            .bg(t.card)
            .border_1()
            .border_color(if matches!(p.state, LinkState::Incoming) { t.accent } else { t.border })
            .child(
                div()
                    .id(("peer-head", hash(&p.id)))
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .cursor_pointer()
                    .child(dot(color))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.0))
                            .flex()
                            .flex_col()
                            .child(
                                div()
                                    .text_size(px(13.0))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(t.text)
                                    .whitespace_nowrap()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .child(p.name.clone()),
                            )
                            .child(div().text_size(px(11.0)).text_color(t.muted).child(status)),
                    )
                    .child(div().text_size(px(11.0)).text_color(t.faint).child(platform))
                    .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.expanded_peer =
                            if this.expanded_peer.as_ref() == Some(&peer_id) { None } else { Some(peer_id.clone()) };
                        cx.notify();
                    })),
            )
            .child(actions)
            .child(media)
            .when(expanded, |d| d.child(details))
    }

    fn settings(&mut self, cx: &mut Context<Self>) -> Div {
        let t = self.theme;
        let Some(state) = self.state.clone() else { return div().flex_1() };
        let row = |label: &'static str| {
            div()
                .flex()
                .items_center()
                .justify_between()
                .gap(px(16.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(t.border)
                .child(div().text_size(px(13.0)).text_color(t.text).child(label))
        };
        let ch = state.update.channel;
        let (seg, items) = segmented("channel", &["Stable", "Dev"], if ch == Channel::Dev { 1 } else { 0 }, &t);
        let items: Vec<AnyElement> = items
            .into_iter()
            .enumerate()
            .map(|(i, it)| {
                it.on_click(cx.listener(move |this, _, _, cx| {
                    let channel = if i == 1 { Channel::Dev } else { Channel::Stable };
                    if let Some(s) = this.state.as_mut() {
                        s.update.channel = channel;
                    }
                    this.cmd(Command::SetChannel { channel });
                    cx.notify();
                }))
                .into_any_element()
            })
            .collect();
        let update_text: SharedString = match &state.update.state {
            UpdateState::Idle | UpdateState::UpToDate => format!("Relay {} · up to date", state.version).into(),
            UpdateState::Checking => "Checking…".into(),
            UpdateState::Available(v) => format!("Relay {v} is available").into(),
            UpdateState::Downloading(p) => format!("Downloading… {:.0}%", p * 100.0).into(),
            UpdateState::Ready(v) => format!("Relay {v} is ready").into(),
            UpdateState::Error(e) => e.clone().into(),
        };
        let update_action = match &state.update.state {
            UpdateState::Ready(_) => Some(
                primary_button("apply-update", "Restart to update", &t)
                    .on_click(cx.listener(|this, _, _, _| this.cmd(Command::ApplyUpdate)))
                    .into_any_element(),
            ),
            UpdateState::Checking | UpdateState::Downloading(_) => None,
            _ => Some(
                button("check-update", "Check now", &t)
                    .on_click(cx.listener(|this, _, _, _| this.cmd(Command::CheckForUpdates)))
                    .into_any_element(),
            ),
        };
        let relay_default = self.state.as_ref().map(|s| s.virtual_audio.is_default).unwrap_or(false);
        let net_on = state.network.enabled;
        div().flex_1().flex().justify_center().p(px(24.0)).child(
            div()
                .w(px(520.0))
                .flex()
                .flex_col()
                .child(row("Updates").child(div().flex().items_center().gap(px(8.0)).child(seg.children(items))))
                .child(
                    row("Version").child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.0))
                            .child(muted(update_text, &t).text_size(px(12.0)))
                            .children(update_action),
                    ),
                )
                .when_some(state.update.rolled_back_from.clone(), |d, v| {
                    d.child(
                        div()
                            .py(px(8.0))
                            .text_size(px(12.0))
                            .text_color(t.warn)
                            .child(format!("Relay {v} didn't start correctly and was rolled back.")),
                    )
                })
                .child(row("Use Relay as the system audio device").child(
                    toggle("relay-default", relay_default, &t).on_click(
                        cx.listener(move |this, _, _, _| this.cmd(Command::MakeRelayDefault { on: !relay_default })),
                    ),
                ))
                .child(row("Find devices on this network").child(
                    toggle("net-on", net_on, &t).on_click(
                        cx.listener(move |this, _, _, _| this.cmd(Command::SetNetworkEnabled { on: !net_on })),
                    ),
                ))
                .child(row("This device").child(muted(state.network.this_name.clone(), &t).text_size(px(12.0))))
                .child(
                    row("Engine").child(
                        muted(
                            format!(
                                "{:.1}% CPU · {:.1} ms blocks · {} late",
                                state.engine.cpu_pct, state.engine.block_ms, state.engine.overruns
                            ),
                            &t,
                        )
                        .text_size(px(12.0)),
                    ),
                ),
        )
    }

    fn menu_items(&self, kind: &MenuKind) -> Vec<(SharedString, Command)> {
        let Some(s) = &self.state else { return Vec::new() };
        let mut items = Vec::new();
        let sources = |items: &mut Vec<(SharedString, Source)>| {
            for d in s.devices.inputs.iter().filter(|d| !d.is_relay) {
                items.push((d.name.clone().into(), Source::Device { key: d.key.clone(), name: d.name.clone() }));
            }
            items.push(("Relay (apps playing to Relay)".into(), Source::Virtual { index: 0 }));
            for app in &s.apps {
                items.push((app.name.clone().into(), Source::App { process: app.process.clone() }));
            }
            for d in s.devices.outputs.iter().filter(|d| !d.is_relay) {
                items.push((
                    format!("{} (everything playing)", d.name).into(),
                    Source::Loopback { key: d.key.clone(), name: d.name.clone() },
                ));
            }
            for p in &s.network.peers {
                for st in p.incoming.iter().filter(|st| st.kind == StreamKind::Audio) {
                    items.push((
                        format!("{} · {}", p.name, st.name).into(),
                        Source::Network { peer: p.id.clone(), stream: st.id },
                    ));
                }
            }
        };
        let sinks = |items: &mut Vec<(SharedString, Sink)>| {
            for d in s.devices.outputs.iter().filter(|d| !d.is_relay) {
                items.push((d.name.clone().into(), Sink::Device { key: d.key.clone(), name: d.name.clone() }));
            }
            items.push(("Relay Mic".into(), Sink::VirtualMic { index: 0 }));
            for p in s.network.peers.iter().filter(|p| matches!(p.state, LinkState::Live | LinkState::Degraded(_))) {
                items.push((format!("Send to {}", p.name).into(), Sink::Network { peer: p.id.clone() }));
            }
        };
        match kind {
            MenuKind::StripSource(id) => {
                let mut v = Vec::new();
                sources(&mut v);
                items.extend(v.into_iter().map(|(l, source)| (l, Command::SetStripSource { id: *id, source })));
                items.push(("Remove input".into(), Command::RemoveStrip { id: *id }));
            }
            MenuKind::AddStrip => {
                let mut v = Vec::new();
                sources(&mut v);
                items.extend(v.into_iter().map(|(l, source)| (l, Command::AddStrip { source })));
            }
            MenuKind::BusSink(id) => {
                let mut v = Vec::new();
                sinks(&mut v);
                items.extend(v.into_iter().map(|(l, sink)| (l, Command::SetBusSink { id: *id, sink })));
                items.push(("Remove output".into(), Command::RemoveBus { id: *id }));
            }
            MenuKind::AddBus => {
                let mut v = Vec::new();
                sinks(&mut v);
                items.extend(v.into_iter().map(|(l, sink)| (l, Command::AddBus { sink })));
            }
        }
        items
    }

    fn menu_overlay(&mut self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let menu = self.menu.as_ref()?;
        let t = self.theme;
        let pos = menu.pos;
        let items = self.menu_items(&menu.kind);
        let list = div()
            .id("menu")
            .occlude()
            .min_w(px(220.0))
            .max_w(px(360.0))
            .max_h(px(420.0))
            .overflow_y_scroll()
            .p(px(4.0))
            .rounded(px(8.0))
            .bg(t.card)
            .border_1()
            .border_color(t.border)
            .shadow_lg()
            .children(items.into_iter().enumerate().map(|(i, (label, command))| {
                let destructive = matches!(command, Command::RemoveStrip { .. } | Command::RemoveBus { .. });
                div()
                    .id(("menu-item", i))
                    .px(px(10.0))
                    .py(px(6.0))
                    .rounded(px(5.0))
                    .text_size(px(12.0))
                    .text_color(if destructive { t.bad } else { t.text })
                    .cursor_pointer()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .text_ellipsis()
                    .hover(|d| d.bg(t.raised))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Command::RemoveStrip { id } | Command::RemoveBus { id } = &command
                            && this
                                .fx
                                .map(|f| matches!(f, Target::Strip(x) | Target::Bus(x) if x == *id))
                                .unwrap_or(false)
                        {
                            this.fx = None;
                        }
                        this.cmd(command.clone());
                        this.menu = None;
                        cx.notify();
                    }))
            }));
        Some(
            deferred(anchored().position(point(pos.x, pos.y + px(6.0))).snap_to_window().child(list))
                .into_any_element(),
        )
    }

    fn sink_label(&self, sink: &Sink) -> String {
        match sink {
            Sink::Network { peer } => {
                let name = self
                    .state
                    .as_ref()
                    .and_then(|s| s.network.peers.iter().find(|p| &p.id == peer).map(|p| p.name.clone()))
                    .unwrap_or_else(|| "a device".into());
                format!("To {name}")
            }
            other => sink_label(other),
        }
    }
}

fn hash(s: &str) -> usize {
    s.bytes().fold(5381usize, |h, b| h.wrapping_mul(33) ^ b as usize)
}

fn theme_for(a: WindowAppearance) -> Theme {
    match a {
        WindowAppearance::Light | WindowAppearance::VibrantLight => Theme::light(),
        _ => Theme::dark(),
    }
}

fn card(id: (&'static str, usize), t: &Theme, selected: bool) -> Stateful<Div> {
    div()
        .id(id)
        .w(px(116.0))
        .h_full()
        .flex_shrink_0()
        .flex()
        .flex_col()
        .gap(px(4.0))
        .p(px(10.0))
        .rounded(px(10.0))
        .bg(t.card)
        .border_1()
        .border_color(if selected { t.accent } else { t.border })
}

fn card_title(name: &str, t: &Theme) -> Div {
    div()
        .text_size(px(13.0))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(t.text)
        .whitespace_nowrap()
        .overflow_hidden()
        .text_ellipsis()
        .child(name.to_string())
}

fn status_line(status: Option<(gpui::Rgba, SharedString)>) -> Div {
    match status {
        Some((color, text)) => div()
            .flex()
            .items_center()
            .gap(px(5.0))
            .text_size(px(10.0))
            .text_color(color)
            .child(dot(color).size(px(6.0)))
            .child(div().overflow_hidden().whitespace_nowrap().text_ellipsis().child(text)),
        None => div().h(px(13.0)),
    }
}

fn fx_active(fx: &Effects) -> bool {
    fx.eq_enabled || fx.gate.enabled || fx.compressor.enabled || fx.limiter.enabled || fx.denoise
}

fn source_label(src: &Source) -> String {
    match src {
        Source::Device { name, .. } => name.clone(),
        Source::Loopback { name, .. } => format!("{name} (all)"),
        Source::Virtual { .. } => "Relay".into(),
        Source::App { process } => process.trim_end_matches(".exe").to_string(),
        Source::Network { .. } => "Network".into(),
        Source::None => "Choose input".into(),
    }
}

fn sink_label(sink: &Sink) -> String {
    match sink {
        Sink::Device { name, .. } => name.clone(),
        Sink::VirtualMic { .. } => "Relay Mic".into(),
        Sink::Network { .. } => "Network".into(),
        Sink::None => "Choose output".into(),
    }
}

impl Render for RelayApp {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let appearance = window.appearance();
        if self.appearance != Some(appearance) {
            self.appearance = Some(appearance);
            self.theme = theme_for(appearance);
        }
        let t = self.theme;
        let body = match self.page {
            Page::Mixer => div()
                .flex_1()
                .min_h(px(0.0))
                .flex()
                .child(div().flex_1().min_w(px(0.0)).flex().flex_col().children(self.banner(cx)).child(self.mixer(cx)))
                .child(self.peers(cx)),
            Page::Settings => div().flex_1().flex().flex_col().child(self.settings(cx)),
        };
        let notice = self.notice.clone();
        div()
            .id("root")
            .size_full()
            .flex()
            .flex_col()
            .bg(t.bg)
            .font_family(ui_font())
            .text_color(t.text)
            .on_mouse_move(cx.listener(Self::on_root_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_root_up))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _: &MouseDownEvent, _, cx| {
                    if this.menu.take().is_some() {
                        cx.notify();
                    }
                }),
            )
            .child(self.header(cx))
            .child(body)
            .children(self.menu_overlay(cx))
            .when_some(notice, |d, (text, error, _)| {
                d.child(deferred(
                    anchored().position(point(px(16.0), window.viewport_size().height - px(56.0))).child(
                        div()
                            .px(px(12.0))
                            .py(px(8.0))
                            .rounded(px(8.0))
                            .bg(t.raised)
                            .border_1()
                            .border_color(if error { t.bad } else { t.border })
                            .text_size(px(12.0))
                            .text_color(t.text)
                            .child(text),
                    ),
                ))
            })
    }
}
