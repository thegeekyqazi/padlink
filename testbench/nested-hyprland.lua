-- Sacrificial Hyprland for padlink tests.
-- Runs as a window inside the real session: no user includes, no autostart,
-- permissions off. Started by nested.sh.
-- Adapted from OpenDisplay's linux/testbench/nested-hyprland.lua (GPL-3.0).

hl.monitor({
    output   = "",
    mode     = "preferred",
    position = "auto",
    scale    = 1,
})

hl.config({
    misc = {
        disable_hyprland_logo = true,
        disable_splash_rendering = true,
    },
})

-- Super+Return opens a terminal inside the nested instance; Super+Shift+E quits.
hl.bind("SUPER + Return", hl.dsp.exec_cmd("foot"))
hl.bind("SUPER + SHIFT + E", hl.dsp.exit())
