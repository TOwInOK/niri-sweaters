## Screen readers

<sup>Since: 25.08</sup>

Niri has basic support for screen readers (specifically, [Orca](https://orca.gnome.org)) when running as a full desktop session, i.e. you need to start niri through a display manager or through `niri-session`.
To avoid conflicts with an already running compositor, niri won't expose accessibility interfaces when started as a nested window, or as a plain `/usr/bin/niri` on a TTY.

We implement the `org.freedesktop.a11y.KeyboardMonitor` D-Bus interface for Orca to listen and grab keyboard keys, and we expose the main niri UI elements via [AccessKit](https://accesskit.dev).
Specifically, niri will announce:

- workspace switching, for example it'll say "Workspace 2" when you switch to the second workspace;
- the exit confirmation dialog (appears on <kbd>Super</kbd><kbd>Shift</kbd><kbd>E</kbd> by default);
- <sup>Since: 25.11</sup> niri has an <kbd>Alt</kbd><kbd>Tab</kbd> window switcher where it will announce the selected window title;
- entering the screenshot UI and the overview (niri will say when these are focused, nothing else for now);
- whenever a config parse error occurs;
- the important hotkeys list (for now, as one big announcement without tab navigation; appears on <kbd>Super</kbd><kbd>Shift</kbd><kbd>/</kbd> by default).

Here's a demo video, watch with sound on.

<video controls src="https://github.com/user-attachments/assets/afceba6f-79f1-47ec-b859-a0fcb7f8eae3">

https://github.com/user-attachments/assets/afceba6f-79f1-47ec-b859-a0fcb7f8eae3

</video>

Make sure [Xwayland](./Xwayland.md) works, then run `orca`.
The default config binds <kbd>Super</kbd><kbd>Alt</kbd><kbd>S</kbd> to toggle Orca, which is the standard key binding.

Note that there are some limitations:

- We don't have a bind to move focus to layer-shell panels. This is not hard to add, but it would be good to have some consensus or prior art with LXQt/Xfce on how exactly this should work.
- You need to have a screen connected and enabled. Without a screen, niri won't give focus any window. This makes sense for sighted users, and I'm not entirely sure what makes the most sense for accessibility purposes (maybe, it'd be better solved with virtual monitors).
- You need working EGL (hardware acceleration).
- We don't have screen curtain functionality yet.

If you're shipping niri and would like to make it work better for screen readers out of the box, consider the following changes to the default niri config:

- Change the default terminal from Alacritty to one that supports screen readers. For example, [GNOME Console](https://gitlab.gnome.org/GNOME/console) or [GNOME Terminal](https://gitlab.gnome.org/GNOME/gnome-terminal) should work well.
- Change the default application launcher and screen locker to ones that support screen readers. For example, [xfce4-appfinder](https://docs.xfce.org/xfce/xfce4-appfinder/start) is an accessible launcher. Suggestions welcome! Likely, something GTK-based will work fine.
- Add some [`spawn-at-startup`](./Configuration:-Miscellaneous.md#spawn-at-startup) command that plays a sound which will indicate to users that niri has finished loading.
- Add `spawn-at-startup "orca"` to run Orca automatically at niri startup.

## Desktop zoom

<sup>Since: next release</sup>

Niri has a built-in desktop zoom that magnifies the rendered desktop without changing the output scale.
Clients are not asked to re-render at a higher scale, so high or fractional zoom levels may look softer.

Desktop zoom is controlled through [bind actions](./Configuration:-Key-Bindings.md#zoom), for example:

```kdl
binds {
    Mod+Z { toggle-zoom 2.0; }
    Mod+X { hold-zoom 2.0; }
}
```

Each output keeps its own zoom state.
Zoom actions apply to the output under the pointer, or to the focused output when the pointer is not on any output.

While zoomed, the viewport follows the pointer.
The [`deadzone-size` setting](./Configuration:-Miscellaneous.md#deadzone-size) defines a centered area of the output in which the pointer can move without moving the zoomed viewport.
Once the pointer leaves the deadzone, the viewport starts following it and keeps moving—even if the pointer itself stops—until the pointer is visually back at the deadzone edge or the viewport reaches the output bounds.
The follow speed grows with the pointer's distance outside the deadzone, between [`follow-min-speed` and `follow-max-speed`](./Configuration:-Miscellaneous.md#follow-min-speed).

Zooming in keeps the point under the pointer visually anchored.
If the pointer is already outside the deadzone, the viewport starts following it during the zoom animation itself, so zooming and following read as one continuous movement.
Zooming back out to 1× returns cleanly to the normal view.

The `zoom-lock` action stops pointer-driven viewport following: while locked, moving the pointer does not pan the view, and the pointer stays confined to the visible area.
Changing the zoom level while locked keeps the content at the center of the viewport centered.

`hold-zoom` is a momentary zoom: pressing the trigger remembers the current view and temporarily zooms in, normal zoom interactions keep working while it is held, and releasing the trigger restores the previous view.

You can also zoom with a touchpad pinch gesture by setting [`pinch-fingers`](./Configuration:-Miscellaneous.md#pinch-fingers) in the `zoom` config section.
This is an opt-in: a matching pinch gesture is claimed by the compositor when it begins, so applications will no longer receive pinch gestures with that finger count.

Opening the [Overview](./Overview.md) temporarily returns the desktop to normal scale; the zoom level is restored when the Overview closes.
The lock screen is shown unzoomed; the stored zoom is restored when you unlock.
Viewport following is also suspended while the screenshot UI or the window switcher is open.

Monitor screenshots and monitor screencasts include desktop zoom.
Window-only screenshots and window casts capture the window unzoomed.

The `zoom` config section also has a [`debug` subsection](./Configuration:-Miscellaneous.md#debug) that can draw the deadzone and the focal point on screen for local tuning.

As an alternative, you can use third-party utilities like [wooz](https://github.com/negrel/wooz).
