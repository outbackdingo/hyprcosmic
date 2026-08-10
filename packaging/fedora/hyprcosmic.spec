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
# WHY IT IS ONE PACKAGE AND NOT TWENTY-SEVEN
# ------------------------------------------
# Fedora splits COSMIC into a package per component, which is right for a
# distribution tracking upstream. This is a fork that replaces the desktop as a
# unit: the compositor, the session and the config compiler are versioned and
# tested together, and there is no supported combination in which you take the
# HyprCosmic cosmic-comp and the distribution's cosmic-session. One package is
# an accurate description of what is actually supported.

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

# These are what "complete replacement" means in packaging terms. Every path
# this package writes under /usr/bin and /usr/share/cosmic is owned by one of
# these on a stock Fedora, so the two cannot be installed at once -- which is
# correct, because they are two builds of the same programs.
#
# Conflicts rather than Obsoletes, deliberately. Obsoletes would let a routine
# `dnf install hyprcosmic` quietly remove the desktop the machine is currently
# running. Conflicts stops and says so, and removing the COSMIC packages stays
# something a person decides to do rather than something a resolver does on
# their behalf.
Conflicts:      cosmic-comp
Conflicts:      cosmic-session

# What it stands in for, so anything depending on a COSMIC session is satisfied.
Provides:       cosmic-comp = %{version}-%{release}
Provides:       cosmic-session = %{version}-%{release}

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
cosmic-conf. The file is the source of truth: keys it names are applied at every
login over whatever the settings UI last stored, and keys it does not name are
left alone.

The shell is waybar, rofi and awww in place of cosmic-panel, cosmic-launcher and
cosmic-bg, and HyDE themes are imported directly. The compositor is COSMIC's,
with a Hyprland-compatible IPC socket so that HyDE's scripts and waybar's
hyprland modules work unmodified.

This package replaces the distribution's COSMIC. It installs both session
entries, so the greeter offers a stock COSMIC shell as well as the HyDE one,
both served by these binaries.

%prep
# Nothing to unpack. See the note at the top of this file.

%install
test -n "%{stagedir}" || { echo "define stagedir: see .github/workflows/packages.yml" >&2; exit 1; }
test -d "%{stagedir}/usr" || { echo "%{stagedir}/usr missing; run just install first" >&2; exit 1; }
cp -a "%{stagedir}/." "%{buildroot}/"

# The desktop entries are the two files a broken install shows up in first, so
# they are validated rather than assumed. A .desktop with a bad Exec line puts
# an entry on the greeter's menu that fails silently when chosen.
desktop-file-validate "%{buildroot}%{_datadir}/wayland-sessions/hyprcosmic.desktop"
desktop-file-validate "%{buildroot}%{_datadir}/wayland-sessions/cosmic.desktop"

# Generated by the workflow from the staged tree rather than written out here.
# A hand-maintained list across 27 components would be wrong within a week, and
# wrong in the direction that ships a package missing files nobody notices until
# a login fails.
%files -f %{filelist}

%changelog
* Sun Aug 10 2026 dingo <outbackdingo@gmail.com> - 0.1.0-1
- First package of the fork: COSMIC replaced as a unit, HyDE shell, cosmic-conf.
