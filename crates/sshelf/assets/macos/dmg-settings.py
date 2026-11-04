# dmgbuild settings for the sshelf .dmg (the release workflow):
#   dmgbuild -s dmg-settings.py -D app=<sshelf.app> -D background=<tiff> "<volume>" <out.dmg>
# The window, icon positions and sizes match examples/dmg_background.rs.
import os.path

app = defines["app"]  # noqa: F821 (dmgbuild provides `defines`)
app_name = os.path.basename(app)

format = "UDZO"
files = [app]
symlinks = {"Applications": "/Applications"}
hide_extensions = [app_name]

background = defines["background"]  # noqa: F821
window_rect = ((200, 120), (640, 400))
default_view = "icon-view"
show_status_bar = False
show_tab_view = False
show_toolbar = False
show_pathbar = False
show_sidebar = False

icon_size = 128
text_size = 13
icon_locations = {
    app_name: (170, 180),
    "Applications": (470, 180),
}
