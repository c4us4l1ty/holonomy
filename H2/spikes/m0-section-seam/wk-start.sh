#!/bin/bash
# Start WebKitWebDriver for the M0 webkit2gtk cross-check.
#
# Three environment problems had to be solved to get a session on this machine:
#
#  1. This system's WebKitWebDriver defaults to /usr/libexec/webkitgtk-6.0/MiniBrowser,
#     but only webkit2gtk **4.1** is installed. Sessions must pass an explicit
#     `webkitgtk:browserOptions.binary` pointing at the 4.1 build.
#
#  2. WebKitGTK spawns its content processes inside a bwrap (glycin) sandbox that
#     cannot start in this container, so the network process died before any page
#     rendered and `POST /session` hung forever.
#     WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1 turns that sandbox off.
#
#  3. There is no X11 socket (XWayland auth exists, the socket does not) and no
#     Xvfb installed, so the X11 backend cannot be used. A real Wayland session
#     is available (gnome-shell, wayland-0), so GDK_BACKEND=wayland is used
#     instead. The review suggested GDK_BACKEND=x11 under xvfb-run; neither Xvfb
#     nor sudo is available here, so the Wayland session is the equivalent.
#
# Not a security concern: this is a local test browser loading a localhost dev
# server, and the sandbox is disabled only so the content process can start.
export WEBKIT_DISABLE_SANDBOX_THIS_IS_DANGEROUS=1
export GDK_BACKEND="${GDK_BACKEND:-wayland}"
export WAYLAND_DISPLAY="${WAYLAND_DISPLAY:-wayland-0}"
export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
export WEBKIT_DISABLE_COMPOSITING_MODE=1
export WEBKIT_DISABLE_DMABUF_RENDERER=1

exec WebKitWebDriver --port="${1:-4444}"
