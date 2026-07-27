//! Transactional Mupen64Plus core, ROM, plugin, and callback preparation.

use super::*;

struct PreparationGuard {
    api: Api,
    core_handle: *mut c_void,
    core_started: bool,
    rom_open: bool,
    plugins: Vec<Plugin>,
}

impl PreparationGuard {
    fn new(api: Api, core_handle: *mut c_void) -> Self {
        Self {
            api,
            core_handle,
            core_started: false,
            rom_open: false,
            plugins: Vec::new(),
        }
    }

    fn finish(mut self) -> (Api, *mut c_void, Vec<Plugin>) {
        let api = self.api;
        let core_handle = self.core_handle;
        let plugins = std::mem::take(&mut self.plugins);
        self.core_started = false;
        self.rom_open = false;
        self.core_handle = ptr::null_mut();
        (api, core_handle, plugins)
    }
}

impl Drop for PreparationGuard {
    fn drop(&mut self) {
        for plugin in self.plugins.drain(..).rev() {
            unsafe {
                let _ = (self.api.core_detach_plugin)(plugin.kind);
                let _ = (plugin.shutdown)();
                libc::dlclose(plugin.handle);
            }
        }
        if self.rom_open {
            unsafe {
                let _ = (self.api.core_do_command)(M64CMD_ROM_CLOSE, 0, ptr::null_mut());
            }
        }
        if self.core_started {
            unsafe {
                let _ = (self.api.core_shutdown)();
            }
        }
        if !self.core_handle.is_null() {
            unsafe {
                libc::dlclose(self.core_handle);
            }
        }
    }
}

impl Mupen64PlusHost {
    pub fn prepare(root: &Path, runtime_home: &Path, rom_path: &Path) -> N64Result<Self> {
        reset_observation_state();
        std::fs::create_dir_all(runtime_home.join("config"))?;
        std::fs::create_dir_all(runtime_home.join("data"))?;
        std::fs::create_dir_all(runtime_home.join("screens"))?;

        let core_path = platform_library(root, "libmupen64plus")?;
        let core_handle = open_library(&core_path)?;
        let api = match unsafe { load_api(core_handle) } {
            Ok(api) => api,
            Err(error) => {
                unsafe { libc::dlclose(core_handle) };
                return Err(error);
            }
        };
        let mut preparation = PreparationGuard::new(api, core_handle);

        let config = path_cstring(&runtime_home.join("config"))?;
        let data = path_cstring(root)?;
        check_core("CoreStartup", unsafe {
            symbol::<CoreStartup>(core_handle, b"CoreStartup\0")?(
                CORE_API_VERSION,
                config.as_ptr(),
                data.as_ptr(),
                ptr::null_mut(),
                debug_log_callback,
                ptr::null_mut(),
                state_callback,
            )
        })?;
        preparation.core_started = true;
        eprintln!("[mupen64plus-native] core started");

        let mut core_section = ptr::null_mut();
        check_core("ConfigOpenSection(Core)", unsafe {
            (api.config_open_section)(cstr(b"Core\0").as_ptr(), &mut core_section)
        })?;
        set_config_int(&api, core_section, b"R4300Emulator\0", M64TYPE_INT, 0)?;
        set_config_int(&api, core_section, b"EnableDebugger\0", M64TYPE_BOOL, 1)?;
        set_config_int(&api, core_section, b"OnScreenDisplay\0", M64TYPE_BOOL, 0)?;
        set_config_string(
            &api,
            core_section,
            b"ScreenshotPath\0",
            &runtime_home.join("screens"),
        )?;

        let rom = std::fs::read(rom_path)?;
        let rom_len = c_int::try_from(rom.len())
            .map_err(|_| N64Error::BadParams("N64 ROM is too large".into()))?;
        check_core("ROM_OPEN", unsafe {
            (api.core_do_command)(M64CMD_ROM_OPEN, rom_len, rom.as_ptr() as *mut c_void)
        })?;
        preparation.rom_open = true;
        eprintln!("[mupen64plus-native] ROM opened");

        let display = display_requested();
        let mut requested_plugins = Vec::with_capacity(3);
        if display {
            requested_plugins.push((M64PLUGIN_GFX, "mupen64plus-video-rice"));
        }
        requested_plugins.push((M64PLUGIN_INPUT, "mupen64plus-input-sdl"));
        requested_plugins.push((M64PLUGIN_RSP, "mupen64plus-rsp-hle"));

        for (kind, stem) in requested_plugins {
            let path = platform_library(root, stem)?;
            eprintln!("[mupen64plus-native] loading plugin {}", path.display());
            let handle = open_library(&path)?;
            eprintln!("[mupen64plus-native] loaded plugin {stem}");
            let startup = match unsafe { symbol::<PluginStartup>(handle, b"PluginStartup\0") } {
                Ok(startup) => startup,
                Err(error) => {
                    unsafe { libc::dlclose(handle) };
                    return Err(error);
                }
            };
            let shutdown = match unsafe { symbol::<PluginShutdown>(handle, b"PluginShutdown\0") } {
                Ok(shutdown) => shutdown,
                Err(error) => {
                    unsafe { libc::dlclose(handle) };
                    return Err(error);
                }
            };
            if let Err(error) = check_core("PluginStartup", unsafe {
                startup(core_handle, ptr::null_mut(), debug_log_callback)
            }) {
                unsafe { libc::dlclose(handle) };
                return Err(error);
            }
            if kind == M64PLUGIN_INPUT {
                if let Err(error) = control::configure_input(&api) {
                    unsafe {
                        let _ = shutdown();
                        libc::dlclose(handle);
                    }
                    return Err(error);
                }
            }
            if let Err(error) = check_core("CoreAttachPlugin", unsafe {
                (api.core_attach_plugin)(kind, handle)
            }) {
                unsafe {
                    let _ = shutdown();
                    libc::dlclose(handle);
                }
                return Err(error);
            }
            preparation.plugins.push(Plugin {
                kind,
                handle,
                shutdown,
            });
            eprintln!("[mupen64plus-native] attached plugin {stem}");
        }

        check_core("DebugSetCallbacks", unsafe {
            (api.debug_set_callbacks)(
                debug_init_callback,
                debug_update_callback,
                debug_vi_callback,
            )
        })?;
        check_core("SET_FRAME_CALLBACK", unsafe {
            (api.core_do_command)(
                M64CMD_SET_FRAME_CALLBACK,
                0,
                frame::frame_callback as *const () as *mut c_void,
            )
        })?;
        eprintln!("[mupen64plus-native] debugger callbacks registered");

        let (api, core_handle, plugins) = preparation.finish();
        Ok(Self {
            api,
            core_handle,
            plugins,
            rom_path: rom_path.to_path_buf(),
            name: std::env::var("EMUCAP_NAME").ok(),
            session_token: std::env::var("EMUCAP_SESSION_TOKEN").ok(),
            launch_id: std::env::var("EMUCAP_LAUNCH_ID").ok(),
            build: std::env::var("EMUCAP_BUILD_HASH").unwrap_or_else(|_| "unknown".into()),
            runtime_home: runtime_home.to_path_buf(),
            display,
            frozen: false,
            frame_paused: false,
            frame_clock_synchronized: false,
            held_buttons: BTreeSet::new(),
            breakpoints: BTreeMap::new(),
            next_breakpoint_id: 1,
            debug_events: VecDeque::new(),
            last_debug_update_seen: 0,
            next_hit_seq: 1,
            next_reset_seq: 1,
            started: false,
        })
    }
}
