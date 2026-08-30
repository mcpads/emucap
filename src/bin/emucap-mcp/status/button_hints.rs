pub(crate) fn button_hint_for_system(system: Option<&str>) -> Option<serde_json::Value> {
    Some(match system? {
        "ss" | "saturn" => serde_json::json!({
            "system": "saturn",
            "buttons": ["a", "b", "c", "x", "y", "z", "l", "r", "start", "up", "down", "left", "right"],
            "aliases": {"ls": "l", "rs": "r", "l1": "l", "r1": "r", "lb": "l", "rb": "r", "enter": "start", "return": "start"},
            "notes": "Saturn pad buttons are lowercase. Directions are up/down/left/right."
        }),
        "psx" | "ps1" | "playstation" => serde_json::json!({
            "system": "psx",
            "buttons": ["cross", "circle", "triangle", "square", "l1", "l2", "r1", "r2", "start", "select", "up", "down", "left", "right"],
            "aliases": {"x": "cross", "o": "circle", "l": "l1", "r": "r1", "enter": "start", "return": "start"},
            "optional": ["l3", "r3"],
            "notes": "Use PlayStation names, not SNES/Saturn a/b."
        }),
        "pce" | "pce_fast" | "pcengine" | "pc-engine" => serde_json::json!({
            "system": "pce",
            "buttons": ["i", "ii", "run", "select", "up", "down", "left", "right"],
            "aliases": {"a": "i", "b": "ii", "start": "run", "enter": "run", "return": "run"},
            "six_button": ["iii", "iv", "v", "vi"],
            "notes": "Prefer PCE button names i/ii/run/select. a/b/start are accepted aliases."
        }),
        "pcfx" | "pc-fx" => serde_json::json!({
            "system": "pcfx",
            "buttons": ["i", "ii", "iii", "iv", "v", "vi", "run", "select", "up", "down", "left", "right"],
            "aliases": {"a": "i", "b": "ii", "start": "run", "enter": "run", "return": "run"},
            "optional": ["mode1", "mode2"],
            "notes": "Prefer PC-FX button names i..vi/run/select. a/b/start are accepted aliases."
        }),
        "md" | "genesis" | "megadrive" | "mega-drive" => serde_json::json!({
            "system": "md",
            "buttons": ["a", "b", "c", "x", "y", "z", "mode", "start", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start"},
            "notes": "Mega Drive/Genesis uses Mednafen md.input.port1=gamepad6 through the launcher so x/y/z/mode are available."
        }),
        "ngp" | "ngpc" | "neo-geo-pocket" | "neo-geo-pocket-color" => serde_json::json!({
            "system": "ngp",
            "buttons": ["a", "b", "option", "up", "down", "left", "right"],
            "aliases": {"start": "option", "enter": "option", "return": "option"},
            "notes": "Neo Geo Pocket and Neo Geo Pocket Color share the Mednafen ngp module and built-in controller."
        }),
        "pc98" => serde_json::json!({
            "system": "pc98",
            "buttons": ["enter", "esc", "space", "up", "down", "left", "right", "backspace", "tab", "del", "ins", "home", "help", "stop", "copy", "shift", "ctrl", "f1", "f2", "f3", "f4", "f5", "f6", "f7", "f8", "f9", "f10", "vf1", "vf2", "vf3", "vf4", "vf5", "a", "b", "c", "d", "e", "f", "g", "h", "i", "j", "k", "l", "m", "n", "o", "p", "q", "r", "s", "t", "u", "v", "w", "x", "y", "z", "0", "1", "2", "3", "4", "5", "6", "7", "8", "9", "kp0", "kp1", "kp2", "kp3", "kp4", "kp5", "kp6", "kp7", "kp8", "kp9"],
            "aliases": {"start": "enter", "return": "enter", "escape": "esc", "select": "space"},
            "notes": "PC-98 uses keyboard inputs through MAME ioport overrides. Ordinary digits and kp0-kp9 keypad inputs are distinct; numpadN, kp_N, and MAME N (pad) spellings are accepted aliases. step(frames) is frame-based, so tap can drive deterministic frozen input."
        }),
        "neogeo_mvs" => serde_json::json!({
            "system": "neogeo_mvs",
            "buttons": ["a", "b", "c", "d", "start", "coin", "service", "up", "down", "left", "right"],
            "notes": "Neo Geo MVS controller port 0. Timed input is released by emulator frame count."
        }),
        "neogeo_aes" => serde_json::json!({
            "system": "neogeo_aes",
            "buttons": ["a", "b", "c", "d", "start", "select", "up", "down", "left", "right"],
            "notes": "Neo Geo AES controller port 0. Cartridge ZIP names select entries in MAME's pinned Neo Geo software list."
        }),
        "neogeo_cd" => serde_json::json!({
            "system": "neogeo_cd",
            "buttons": ["a", "b", "c", "d", "start", "select", "up", "down", "left", "right"],
            "notes": "Neo Geo CD controller port 0. CD media uses a CUE entry file and all referenced tracks."
        }),
        "dc" | "dreamcast" => serde_json::json!({
            "system": "dreamcast",
            "buttons": ["a", "b", "c", "x", "y", "z", "d", "start", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start"},
            "notes": "Dreamcast pad buttons are lowercase: a/b/x/y/start + up/down/left/right are standard; c/z/d exist on some pads. Analog triggers/stick are not injectable by name. Input is injected at the maple GetInput consumer; only controller port 0 is supported."
        }),
        "gamecube" | "gc" | "ngc" => serde_json::json!({
            "system": "gamecube",
            "buttons": ["a", "b", "x", "y", "z", "l", "r", "start", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r"},
            "notes": "GameCube controller button names are lowercase. Only controller port 0 is supported by the native adapter."
        }),
        "wii" | "nintendo-wii" => serde_json::json!({
            "system": "wii",
            "buttons": ["a", "b", "one", "two", "minus", "plus", "home", "up", "down", "left", "right"],
            "notes": "Emulated Wii Remote 1 core buttons only. IR, motion, Nunchuk, Classic Controller, and other extension inputs are not injectable."
        }),
        "xbox" | "original-xbox" | "ogxbox" => serde_json::json!({
            "system": "xbox",
            "buttons": ["a", "b", "x", "y", "white", "black", "start", "back", "up", "down", "left", "right", "l", "r", "lstick", "rstick"],
            "aliases": {"select": "back", "lt": "l", "rt": "r", "l3": "lstick", "r3": "rstick", "enter": "start", "return": "start"},
            "notes": "Original Xbox controller port 0. l/r are full-trigger aliases; input_control(operation=describe) reports live analog-stick and partial-trigger axes when available."
        }),
        "snes" | "sfc" => serde_json::json!({
            "system": "snes",
            "buttons": ["a", "b", "x", "y", "l", "r", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r", "lb": "l", "rb": "r"},
            "notes": "Mesen SNES uses lowercase SNES button names."
        }),
        "gamegear" | "gg" | "sms" => serde_json::json!({
            "system": "gamegear",
            "buttons": ["up", "down", "left", "right", "one", "two", "pause"],
            "aliases": {"start": "pause", "enter": "pause", "return": "pause", "a": "two", "b": "one", "1": "one", "2": "two", "button1": "one", "button2": "two"},
            "notes": "Mesen Game Gear (SMS controller): one=Button1(B), two=Button2(A), pause=Start. Aliases let you use start/a/b/1/2."
        }),
        "gb" | "gbc" | "gameboy" | "game-boy" | "dmg" | "gbcolor" | "gameboycolor" | "cgb" => {
            serde_json::json!({
                "system": "gb",
                "buttons": ["a", "b", "start", "select", "up", "down", "left", "right"],
                "aliases": {"enter": "start", "return": "start"},
                "notes": "Mesen Game Boy / Game Boy Color (gameboy console): a/b/start/select + directions, lowercase. No X/Y/L/R."
            })
        }
        "gba" | "gameboyadvance" | "game-boy-advance" | "agb" => serde_json::json!({
            "system": "gba",
            "buttons": ["a", "b", "l", "r", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r", "lb": "l", "rb": "r"},
            "notes": "Mesen Game Boy Advance (ARM7): a/b/l/r/start/select + directions, lowercase. No X/Y."
        }),
        "nes" | "famicom" | "fc" | "nintendo" => serde_json::json!({
            "system": "nes",
            "buttons": ["a", "b", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start"},
            "notes": "Mesen NES / Famicom (nes console / 6502-2A03 CPU): a/b/start/select + directions, lowercase. No X/Y/L/R."
        }),
        "msx" | "msx2+" | "msx1" | "msx2" | "msx2p" => serde_json::json!({
            "system": system,
            "buttons": ["space", "ctrl", "enter", "esc", "a", "b", "up", "down", "left", "right"],
            "aliases": {"start": "enter", "return": "enter", "fire1": "space", "fire2": "ctrl"},
            "devices": [
                {"port": 0, "device": "keyboard", "buttons": ["space", "ctrl", "enter", "esc", "a", "b", "up", "down", "left", "right"]},
                {"port": 1, "device": "joystick", "buttons": ["a", "b", "up", "down", "left", "right"], "aliases": {"fire1": "a", "fire2": "b", "button1": "a", "button2": "b"}},
                {"port": 2, "device": "joystick", "buttons": ["a", "b", "up", "down", "left", "right"], "aliases": {"fire1": "a", "fire2": "b", "button1": "a", "button2": "b"}}
            ],
            "notes": "Port 0 injects the standard keyboard matrix. Ports 1 and 2 are independent active-low joysticks. An empty set returns the selected device to native input."
        }),
        "nds" | "ds" | "nintendo-ds" => serde_json::json!({
            "system": "nds",
            "buttons": ["a", "b", "x", "y", "l", "r", "start", "select", "up", "down", "left", "right"],
            "aliases": {"enter": "start", "return": "start", "l1": "l", "r1": "r"},
            "notes": "Nintendo DS buttons are injected through the DeSmuME bridge; only controller port 0 is supported. Use the dedicated touch tool for the lower screen. Microphone input is not injectable."
        }),
        // Unknown systems do not borrow another controller's vocabulary.
        _ => return None,
    })
}
