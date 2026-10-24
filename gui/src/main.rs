//! tuxedo-gui: a small libadwaita window for TUXEDO Uniwill fan/performance control.
//!
//! Runs as your normal user; all hardware access goes through tuxedo-controld over the Unix
//! socket /run/tuxedo-control.sock. Live status + performance profile + fan auto/manual.

use std::cell::{Cell, RefCell};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde::Deserialize;

mod curve_editor;

const SOCK: &str = "/run/tuxedo-control.sock";

#[derive(Deserialize, Default, Clone)]
struct Status {
    cpu_temp: i32,
    gpu_temp: i32,
    cpu_fan: i32,
    gpu_fan: i32,
    // (the daemon also sends mode/curve_pct; serde ignores fields we don't use)
    manual: Option<i32>,
    profile: String,
    #[serde(default)]
    kbd: i32,
    #[serde(default)]
    kbd_max: i32,
    #[serde(default)]
    charge: String,
    #[serde(default)]
    charges: String,
}

/// Send one command line to the daemon, return the response line (None if daemon down).
fn send(cmd: &str) -> Option<String> {
    let mut s = UnixStream::connect(SOCK).ok()?;
    s.set_read_timeout(Some(Duration::from_millis(800))).ok()?;
    s.write_all(cmd.as_bytes()).ok()?;
    s.write_all(b"\n").ok()?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).ok()?;
    Some(line.trim().to_string())
}
fn fetch_status() -> Option<Status> {
    serde_json::from_str(&send("STATUS")?).ok()
}

#[derive(Deserialize, Default, Clone)]
struct ProfileMeta {
    name: String,
    #[serde(default)]
    perf: String,
    #[serde(default)]
    curve: Vec<(i32, i32)>,
}
#[derive(Deserialize, Default, Clone)]
struct ProfileList {
    active: String,
    #[allow(dead_code)]
    default: String,
    profiles: Vec<ProfileMeta>,
}
fn fetch_profiles() -> Option<ProfileList> {
    serde_json::from_str(&send("LISTPROFILES")?).ok()
}

const PROFILES: [(&str, &str); 3] = [
    ("power_save", "Power Save"),
    ("enthusiast", "Balanced"),
    ("overboost", "Performance"),
];
// Charging profiles, ordered low→high capacity (matches the native app's Stationary/Balanced/Max).
const CHARGES: [(&str, &str); 3] = [
    ("stationary", "Stationary (~60%)"),
    ("balanced", "Balanced (~80%)"),
    ("high_capacity", "High Capacity (100%)"),
];
// Built-in profiles cannot be deleted (mirrors the daemon).
const BUILTINS: [&str; 4] = ["Max Energy Save", "Quiet", "Office", "High Performance"];

fn build_ui(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("TUXEDO Control")
        .default_width(420)
        .default_height(540)
        .build();

    let toasts = adw::ToastOverlay::new();
    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // Header bar with an Appearance menu (System / Light / Dark). libadwaita follows the system
    // colour-scheme by default; these let the user override per-app.
    let header = adw::HeaderBar::new();
    let menu = gio::Menu::new();
    // Profile management lives in the Profile section (below), not this menu.
    let appearance = gio::Menu::new();
    appearance.append(Some("Follow system"), Some("app.theme::system"));
    appearance.append(Some("Light"), Some("app.theme::light"));
    appearance.append(Some("Dark"), Some("app.theme::dark"));
    menu.append_section(Some("Appearance"), &appearance);
    // Standard primary-menu items (GNOME HIG). Preferences/Shortcuts/Help omitted: the
    // window itself is the settings, and there are no shortcut/help pages yet.
    let about_section = gio::Menu::new();
    about_section.append(Some("About TUXEDO Control"), Some("app.about"));
    menu.append_section(None, &about_section);
    let menu_btn = gtk::MenuButton::builder()
        .icon_name("open-menu-symbolic")
        .menu_model(&menu)
        .tooltip_text("Menu")
        .build();
    header.pack_end(&menu_btn);
    root.append(&header);

    // Stateful theme action. Default = follow the system colour-scheme.
    let theme_action = gio::SimpleAction::new_stateful(
        "theme",
        Some(glib::VariantTy::STRING),
        &"system".to_variant(),
    );
    theme_action.connect_activate(|action, param| {
        if let Some(v) = param.and_then(|p| p.get::<String>()) {
            adw::StyleManager::default().set_color_scheme(match v.as_str() {
                "light" => adw::ColorScheme::ForceLight,
                "dark" => adw::ColorScheme::ForceDark,
                _ => adw::ColorScheme::Default,
            });
            action.set_state(&v.to_variant());
        }
    });
    app.add_action(&theme_action);

    // About dialog (standard primary-menu item).
    {
        let window = window.clone();
        let about = gio::SimpleAction::new("about", None);
        about.connect_activate(move |_, _| {
            adw::AboutWindow::builder()
                .transient_for(&window)
                .application_name("TUXEDO Control")
                .application_icon("preferences-system")
                .developer_name("Andreas Demosthenous")
                .version(env!("CARGO_PKG_VERSION"))
                .comments(
                    "Declarative fan and performance control for TUXEDO Uniwill laptops, \
                     via the tuxedo_io kernel interface.",
                )
                .website("https://github.com/AndrewDemsDS/tuxedo-control")
                .issue_url("https://github.com/AndrewDemsDS/tuxedo-control/issues")
                .license_type(gtk::License::MitX11)
                .developers(vec!["Andreas Demosthenous".to_string()])
                .copyright("© 2026 Andreas Demosthenous")
                .build()
                .present();
        });
        app.add_action(&about);
    }

    let clamp = adw::Clamp::builder()
        .maximum_size(440)
        .margin_top(12)
        .margin_bottom(12)
        .margin_start(12)
        .margin_end(12)
        .build();
    let page = gtk::Box::new(gtk::Orientation::Vertical, 18);

    // ---- Profiles (named bundles: perf + fan curve + kbd + charge) ----
    let prof_group = adw::PreferencesGroup::builder()
        .title("Profile")
        .description("Switch the active profile, or manage them here")
        .build();
    // Management buttons in the group header (use the app actions wired below).
    let mgmt = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    mgmt.add_css_class("linked");
    let new_btn = gtk::Button::from_icon_name("list-add-symbolic");
    new_btn.set_tooltip_text(Some("New profile"));
    new_btn.set_action_name(Some("app.newprofile"));
    let del_btn = gtk::Button::from_icon_name("user-trash-symbolic");
    del_btn.set_tooltip_text(Some("Delete current profile"));
    del_btn.set_action_name(Some("app.delprofile"));
    let star_btn = gtk::Button::from_icon_name("starred-symbolic");
    star_btn.set_tooltip_text(Some("Set current as default"));
    star_btn.set_action_name(Some("app.setdefault"));
    let import_btn = gtk::Button::from_icon_name("document-import-symbolic");
    import_btn.set_tooltip_text(Some("Import TUXEDO Control Center profiles"));
    import_btn.set_action_name(Some("app.importtcc"));
    mgmt.append(&new_btn);
    mgmt.append(&del_btn);
    mgmt.append(&star_btn);
    mgmt.append(&import_btn);
    prof_group.set_header_suffix(Some(&mgmt));
    let prof_combo = adw::ComboRow::builder().title("Active profile").build();
    let prof_model = gtk::StringList::new(&[]);
    prof_combo.set_model(Some(&prof_model));
    prof_group.add(&prof_combo);
    let curve_row = adw::ActionRow::builder()
        .title("Fan curve")
        .subtitle("Edit the active profile's temperature → fan-speed curve")
        .build();
    let edit_curve_btn = gtk::Button::builder()
        .label("Edit…")
        .valign(gtk::Align::Center)
        .build();
    curve_row.add_suffix(&edit_curve_btn);
    curve_row.set_activatable_widget(Some(&edit_curve_btn));
    prof_group.add(&curve_row);
    page.append(&prof_group);
    let prof_names: Rc<RefCell<Vec<String>>> = Rc::new(RefCell::new(Vec::new()));

    // ---- Status group ----
    let status_group = adw::PreferencesGroup::builder().title("Status").build();
    let cpu_row = adw::ActionRow::builder().title("CPU").build();
    let gpu_row = adw::ActionRow::builder().title("GPU").build();
    let cpu_val = gtk::Label::new(Some("n/a"));
    let gpu_val = gtk::Label::new(Some("n/a"));
    cpu_val.add_css_class("dim-label");
    gpu_val.add_css_class("dim-label");
    cpu_row.add_suffix(&cpu_val);
    gpu_row.add_suffix(&gpu_val);
    status_group.add(&cpu_row);
    status_group.add(&gpu_row);
    page.append(&status_group);

    // ---- Performance profile ----
    let perf_group = adw::PreferencesGroup::builder()
        .title("Performance profile")
        .build();
    let perf_row = adw::ComboRow::builder().title("Profile").build();
    let model = gtk::StringList::new(&PROFILES.iter().map(|p| p.1).collect::<Vec<_>>());
    perf_row.set_model(Some(&model));
    perf_group.add(&perf_row);
    page.append(&perf_group);

    // ---- Fan ----
    let fan_group = adw::PreferencesGroup::builder().title("Fan").build();
    let auto_row = adw::ActionRow::builder()
        .title("Automatic")
        .subtitle("Follow the temperature curve")
        .build();
    let auto_switch = gtk::Switch::builder()
        .valign(gtk::Align::Center)
        .active(true)
        .build();
    auto_row.add_suffix(&auto_switch);
    auto_row.set_activatable_widget(Some(&auto_switch));

    let manual_row = adw::ActionRow::builder().title("Manual speed").build();
    let scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 100.0, 5.0);
    scale.set_hexpand(true);
    scale.set_size_request(200, -1);
    scale.set_draw_value(true);
    scale.set_value_pos(gtk::PositionType::Right);
    manual_row.add_suffix(&scale);
    manual_row.set_sensitive(false);
    fan_group.add(&auto_row);
    fan_group.add(&manual_row);
    page.append(&fan_group);

    // ---- Keyboard backlight (shown only if the hardware has one) ----
    let kbd_group = adw::PreferencesGroup::builder()
        .title("Keyboard backlight")
        .visible(false)
        .build();
    let kbd_row = adw::ActionRow::builder().title("Brightness").build();
    let kbd_scale = gtk::Scale::with_range(gtk::Orientation::Horizontal, 0.0, 4.0, 1.0);
    kbd_scale.set_size_request(200, -1);
    kbd_scale.set_draw_value(true);
    kbd_scale.set_value_pos(gtk::PositionType::Right);
    kbd_row.add_suffix(&kbd_scale);
    kbd_group.add(&kbd_row);
    page.append(&kbd_group);

    // ---- Battery charging profile (shown only if supported) ----
    let chg_group = adw::PreferencesGroup::builder()
        .title("Battery")
        .visible(false)
        .build();
    let chg_row = adw::ComboRow::builder()
        .title("Charging profile")
        .subtitle("Cap charging to extend battery lifespan")
        .build();
    let chg_model = gtk::StringList::new(&CHARGES.iter().map(|c| c.1).collect::<Vec<_>>());
    chg_row.set_model(Some(&chg_model));
    chg_group.add(&chg_row);
    page.append(&chg_group);

    clamp.set_child(Some(&page));
    // Scroll the content so a small window stays usable. (No propagate_natural_height:
    // that would size the scroller to its content and defeat scrolling.)
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .min_content_height(200)
        .child(&clamp)
        .build();
    root.append(&scroller);
    toasts.set_child(Some(&root));
    window.set_content(Some(&toasts));

    let toast = {
        let toasts = toasts.clone();
        move |m: &str| toasts.add_toast(adw::Toast::new(m))
    };
    // Guard so programmatic widget updates (from polling) don't re-fire command handlers.
    let updating = Rc::new(Cell::new(false));

    // Profile change -> daemon
    {
        let updating = updating.clone();
        let toast = toast.clone();
        perf_row.connect_selected_notify(move |row| {
            if updating.get() {
                return;
            }
            let idx = row.selected() as usize;
            if let Some((id, label)) = PROFILES.get(idx) {
                match send(&format!("PROFILE {id}")).as_deref() {
                    Some("OK") => toast(&format!("Profile: {label}")),
                    _ => toast("Couldn't set profile (daemon down?)"),
                }
            }
        });
    }
    // Auto switch -> daemon + enable/disable manual row
    {
        let updating = updating.clone();
        let manual_row = manual_row.clone();
        let scale = scale.clone();
        let toast = toast.clone();
        auto_switch.connect_active_notify(move |sw| {
            manual_row.set_sensitive(!sw.is_active());
            if updating.get() {
                return;
            }
            if sw.is_active() {
                let _ = send("FANAUTO");
                toast("Fan: automatic");
            } else {
                let _ = send(&format!("FANMANUAL {}", scale.value() as i32));
                toast("Fan: manual");
            }
        });
    }
    // Manual slider -> daemon (only when manual mode active)
    {
        let updating = updating.clone();
        let auto_switch = auto_switch.clone();
        scale.connect_value_changed(move |s| {
            if updating.get() || auto_switch.is_active() {
                return;
            }
            let _ = send(&format!("FANMANUAL {}", s.value() as i32));
        });
    }

    // Keyboard backlight slider -> daemon
    {
        let updating = updating.clone();
        kbd_scale.connect_value_changed(move |s| {
            if updating.get() {
                return;
            }
            let _ = send(&format!("KBDSET {}", s.value() as i32));
        });
    }
    // Charging profile -> daemon
    {
        let updating = updating.clone();
        let toast = toast.clone();
        chg_row.connect_selected_notify(move |row| {
            if updating.get() {
                return;
            }
            if let Some((id, label)) = CHARGES.get(row.selected() as usize) {
                match send(&format!("CHARGE {id}")).as_deref() {
                    Some("OK") => toast(&format!("Charging: {label}")),
                    _ => toast("Couldn't set charging profile"),
                }
            }
        });
    }

    // Active-profile combo -> ACTIVATE
    {
        let updating = updating.clone();
        let prof_names = prof_names.clone();
        let toast = toast.clone();
        prof_combo.connect_selected_notify(move |row| {
            if updating.get() {
                return;
            }
            if let Some(name) = prof_names.borrow().get(row.selected() as usize) {
                match send(&format!("ACTIVATE {name}")).as_deref() {
                    Some("OK") => toast(&format!("Activated: {name}")),
                    _ => toast("Couldn't activate profile"),
                }
            }
        });
    }

    // Edit fan curve of the active profile -> graphical editor -> SAVEPROFILE.
    {
        let app = app.clone();
        let toast = toast.clone();
        edit_curve_btn.connect_clicked(move |_| {
            let Some(pl) = fetch_profiles() else {
                toast("Daemon not running");
                return;
            };
            let Some(p) = pl.profiles.iter().find(|p| p.name == pl.active).cloned() else {
                return;
            };
            let toast = toast.clone();
            curve_editor::open(
                &app,
                &p.name,
                p.perf.clone(),
                p.curve.clone(),
                move |name, perf, curve| {
                    let pts: Vec<String> =
                        curve.iter().map(|(t, d)| format!("[{t},{d}]")).collect();
                    let json = format!(
                        "{{\"name\":\"{name}\",\"perf\":\"{perf}\",\"curve\":[{}]}}",
                        pts.join(",")
                    );
                    // SAVEPROFILE replaces the stored profile; the loop reads the active curve live.
                    match send(&format!("SAVEPROFILE {json}")).as_deref() {
                        Some("OK") => toast(&format!("Saved curve: {name}")),
                        _ => toast("Couldn't save curve"),
                    }
                },
            );
        });
    }

    // Profile-management actions (header menu).
    {
        let window = window.clone();
        let toast = toast.clone();
        let act = gio::SimpleAction::new("newprofile", None);
        act.connect_activate(move |_, _| {
            let dialog = adw::MessageDialog::new(Some(&window), Some("New profile"), Some("Create a profile with a balanced curve; tweak it after."));
            let entry = gtk::Entry::builder().placeholder_text("Profile name").build();
            dialog.set_extra_child(Some(&entry));
            dialog.add_responses(&[("cancel", "Cancel"), ("create", "Create")]);
            dialog.set_response_appearance("create", adw::ResponseAppearance::Suggested);
            dialog.set_default_response(Some("create"));
            let toast = toast.clone();
            dialog.connect_response(None, move |d, resp| {
                if resp == "create" {
                    let name = entry.text().to_string().replace('"', "");
                    if !name.is_empty() {
                        let json = format!("{{\"name\":\"{name}\",\"perf\":\"enthusiast\",\"curve\":[[25,0],[50,0],[62,24],[80,60],[90,100]]}}");
                        if send(&format!("SAVEPROFILE {json}")).as_deref() == Some("OK") { toast(&format!("Created: {name}")); }
                        else { toast("Couldn't create profile"); }
                    }
                }
                d.close();
            });
            dialog.present();
        });
        app.add_action(&act);
    }
    {
        let toast = toast.clone();
        let act = gio::SimpleAction::new("delprofile", None);
        act.connect_activate(move |_, _| {
            if let Some(pl) = fetch_profiles() {
                let _ = send(&format!("DELPROFILE {}", pl.active));
                toast(&format!("Deleted: {}", pl.active));
            }
        });
        app.add_action(&act);
    }
    {
        let toast = toast.clone();
        let act = gio::SimpleAction::new("setdefault", None);
        act.connect_activate(move |_, _| {
            if let Some(pl) = fetch_profiles() {
                let _ = send(&format!("SETDEFAULT {}", pl.active));
                toast(&format!("Default: {}", pl.active));
            }
        });
        app.add_action(&act);
    }
    {
        let window = window.clone();
        let toast = toast.clone();
        let act = gio::SimpleAction::new("importtcc", None);
        act.connect_activate(move |_, _| {
            let dialog = gtk::FileDialog::builder()
                .title("Import TUXEDO Control Center profiles")
                .build();
            let toast = toast.clone();
            dialog.open(Some(&window), gio::Cancellable::NONE, move |res| {
                if let Ok(file) = res {
                    if let Some(path) = file.path() {
                        let r = send(&format!("IMPORTTCC {}", path.display())).unwrap_or_default();
                        toast(if r.starts_with("OK") {
                            "Imported TUXEDO profiles"
                        } else {
                            "Import failed"
                        });
                    }
                }
            });
        });
        app.add_action(&act);
    }

    // Poll the profile list (changes rarely) every 2s.
    {
        let updating = updating.clone();
        let prof_names = prof_names.clone();
        let del_btn = del_btn.clone();
        glib::timeout_add_local(Duration::from_millis(2000), move || {
            if let Some(pl) = fetch_profiles() {
                let names: Vec<String> = pl.profiles.iter().map(|p| p.name.clone()).collect();
                if *prof_names.borrow() != names {
                    updating.set(true);
                    let strs: Vec<&str> = names.iter().map(String::as_str).collect();
                    prof_model.splice(0, prof_model.n_items(), &strs);
                    *prof_names.borrow_mut() = names.clone();
                    updating.set(false);
                }
                // Built-in profiles can't be deleted.
                del_btn.set_sensitive(!BUILTINS.contains(&pl.active.as_str()));
                if let Some(i) = names.iter().position(|n| *n == pl.active) {
                    if prof_combo.selected() as usize != i {
                        updating.set(true);
                        prof_combo.set_selected(i as u32);
                        updating.set(false);
                    }
                }
            }
            glib::ControlFlow::Continue
        });
    }

    // Poll status into the widgets every 1.5s.
    {
        let updating = updating.clone();
        glib::timeout_add_local(Duration::from_millis(1500), move || {
            if let Some(st) = fetch_status() {
                updating.set(true);
                cpu_val.set_text(&format!("{} °C  ·  fan {}%", st.cpu_temp, st.cpu_fan));
                gpu_val.set_text(&format!("{} °C  ·  fan {}%", st.gpu_temp, st.gpu_fan));
                if let Some(i) = PROFILES.iter().position(|(id, _)| *id == st.profile) {
                    if perf_row.selected() as usize != i {
                        perf_row.set_selected(i as u32);
                    }
                }
                let is_auto = st.manual.is_none();
                if auto_switch.is_active() != is_auto {
                    auto_switch.set_active(is_auto);
                }
                manual_row.set_sensitive(!is_auto);
                if let Some(m) = st.manual {
                    if scale.value() as i32 != m {
                        scale.set_value(m as f64);
                    }
                }

                // Keyboard backlight (only if present)
                if st.kbd_max > 0 {
                    kbd_group.set_visible(true);
                    if (kbd_scale.adjustment().upper() as i32) != st.kbd_max {
                        kbd_scale.set_range(0.0, st.kbd_max as f64);
                    }
                    if st.kbd >= 0 && kbd_scale.value() as i32 != st.kbd {
                        kbd_scale.set_value(st.kbd as f64);
                    }
                } else {
                    kbd_group.set_visible(false);
                }

                // Charging profile (only if supported)
                if !st.charges.is_empty() {
                    chg_group.set_visible(true);
                    if let Some(i) = CHARGES.iter().position(|(id, _)| *id == st.charge) {
                        if chg_row.selected() as usize != i {
                            chg_row.set_selected(i as u32);
                        }
                    }
                } else {
                    chg_group.set_visible(false);
                }
                updating.set(false);
            } else {
                cpu_val.set_text("daemon not running");
                gpu_val.set_text("n/a");
            }
            glib::ControlFlow::Continue
        });
    }

    window.present();
}

fn main() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id("xyz.homecyta.TuxedoControl")
        .build();
    app.connect_activate(build_ui);
    app.run()
}
