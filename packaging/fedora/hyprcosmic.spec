# HyprCosmic, as one RPM.
#
# WHY THIS REPACKS RATHER THAN REBUILDS
# -------------------------------------
# There is no %build here. The spec packages a tree that `just install` has
# already produced, which .github/workflows/packages.yml stages and passes in as
# --define "stagedir ...".
#
# The alternative -- a spec that runs `just build` itself under rpmbuild -- is
# the more orthodox shape and is wrong for this project. `just build` compiles
# 27 Rust components; doing it a second time inside rpmbuild to produce bytes
# identical to the ones already sitting in the tree costs hours and buys
# nothing. Worse, it would let the packaged desktop and the `just install`
# desktop drift apart, and the whole point of shipping a package is that the
# two are the same thing.
#
# The cost of this choice, stated plainly: the resulting RPM is only valid on
# the distribution it was built on. Nothing here is statically linked, so an RPM
# built on Fedora 44 assumes Fedora 44's glibc, wayland, libinput and mesa. Build
# it on the release you intend to install it on. The workflow does that by
# running the whole job inside a container of the target distribution.
#
# WHY IT IS ONE PACKAGE, AND WHY IT IS A SMALL ONE
# ------------------------------------------------
# Fedora splits COSMIC into a package per component, which is right for a
# distribution tracking upstream. This fork changes three of them -- the
# compositor, the session and the config compiler it adds -- and they are
# versioned and tested together, so one package is an accurate description of
# what is actually supported.
#
# It is not a package per component and it is not the whole desktop either. The
# build tree produces all of COSMIC, because it is COSMIC's tree, but shipping
# all of it would mean owning files that 25 distribution packages already own.
# The workflow reduces the staged tree to what this fork actually produces
# before any of the three packaging recipes see it.

%global _hardened_build 1

# Debuginfo extraction re-links every binary in the tree and would add an hour
# to a package whose binaries were built elsewhere anyway. There is nothing to
# strip usefully here.
%global debug_package %{nil}

Name:           hyprcosmic
Version:        %{?ver}%{!?ver:0.1.0}
Release:        %{?rel}%{!?rel:1}%{?dist}
Summary:        COSMIC configured in Hyprland's idiom, with a HyDE shell

License:        GPL-3.0-only
URL:            https://github.com/outbackdingo/hyprcosmic
BuildArch:      x86_64

# COSMIC itself, which this runs on rather than replaces.
#
# One line pulls the whole desktop, because cosmic-session requires every
# component. That is exactly what is wanted: HyprCosmic forks the compositor,
# the session and adds the config compiler, and takes cosmic-settings,
# cosmic-osd, the portal and the rest from the distribution at the version the
# distribution tested them at.
#
# There is deliberately no Conflicts and no Provides here. An earlier revision
# had both, on the reading that a fork of the desktop replaces the desktop, and
# it could not be installed: this package's files collided with 25 others in
# rpm's transaction check, and satisfying that by claiming all 25 with Conflicts
# would have erased cosmic-greeter, which on a stock Fedora COSMIC is the
# display manager. Installing beside COSMIC costs three renamed binaries and
# leaves the stock session on the greeter's menu to fall back to.
Requires:       cosmic-session >= 1.5.0

# The HyDE shell. These are separate programs this fork drives rather than
# builds, and without them the session starts to a blank screen with no bar and
# no launcher.
Requires:       waybar
Requires:       rofi-wayland

# The wallpaper daemon. Recommends rather than Requires because it lives in the
# alebastr/sway-extras COPR rather than in Fedora proper, and a hard dependency
# that cannot resolve would make this package uninstallable on a machine that
# has not enabled that repository. Without it you get no wallpaper; with it and
# no theme imported, you also get no wallpaper. Both are recoverable; an
# unsatisfiable dependency is not.
Recommends:     awww

# Nerd Font glyphs are most of what the bar draws.
Recommends:     nerd-fonts

%description
HyprCosmic is a fork of the COSMIC desktop that takes its configuration in
Hyprland's idiom and wears a HyDE-style shell.

A single ~/.config/hyprcosmic/cosmic.conf -- with general { } blocks, bind =
lines and $variables -- is compiled into COSMIC's own configuration tree by
hyprcosmic-conf. The file is the source of truth: keys it names are applied at every
login over whatever the settings UI last stored, and keys it does not name are
left alone.

The shell is waybar, rofi and awww in place of cosmic-panel, cosmic-launcher and
cosmic-bg, and HyDE themes are imported directly. The compositor is COSMIC's,
with a Hyprland-compatible IPC socket so that HyDE's scripts and waybar's
hyprland modules work unmodified.

This package installs beside the distribution's COSMIC rather than over it. Its
binaries are hyprcosmic-comp, hyprcosmic-session and hyprcosmic-conf, and it
adds one session entry; the stock COSMIC entry stays on the greeter's menu,
served by the distribution's own binaries, so a session that will not start is
one logout away from a desktop that will.

%prep
# Nothing to unpack. See the note at the top of this file.

%install
test -n "%{stagedir}" || { echo "define stagedir: see .github/workflows/packages.yml" >&2; exit 1; }
test -d "%{stagedir}/usr" || { echo "%{stagedir}/usr missing; run just install first" >&2; exit 1; }
cp -a "%{stagedir}/." "%{buildroot}/"

# The session entry is checked by the workflow against the staged tree, not
# with desktop-file-validate here. desktop-file-validate rejects DesktopNames,
# the key a display manager reads to set XDG_CURRENT_DESKTOP, because the
# Desktop Entry Specification registers keys for application launchers and this
# is a session file. The copy Fedora already ships as cosmic-session-1.5.0-1.fc44
# fails the identical check -- so this is the validator's gap, not something the
# fork introduced. Dropping the key would satisfy the validator and break the
# session.
# See "Check the session entries" in .github/workflows/packages.yml, which
# tests what actually matters: that Exec names a file this package installs.

# Generated by the workflow from the staged tree rather than written out here.
# A hand-maintained list across 27 components would be wrong within a week, and
# wrong in the direction that ships a package missing files nobody notices until
# a login fails.
%files -f %{filelist}

%changelog
* Mon Aug 10 2026 dingo <outbackdingo@gmail.com> - 0.1.0-1
- First package of the fork: hyprcosmic-comp, hyprcosmic-session and
  hyprcosmic-conf installed beside the distribution's COSMIC, with a HyDE shell.
