set dotenv-load
just := just_executable()
make := `which make`

build:
    mkdir -p build
    {{ just }} cosmic-applets/build-release
    {{ just }} cosmic-applibrary/build-release
    {{ just }} cosmic-bg/build-release
    {{ make }} -C cosmic-comp all
    # cargo directly, not `just cosmic-conf/build-release`: cosmic-conf is not a
    # submodule, it is a crate in this repository, and it has no Justfile of its
    # own to delegate to.
    cargo build --release --manifest-path cosmic-conf/Cargo.toml
    {{ just }} cosmic-edit/build-release
    {{ just }} cosmic-files/build-release
    {{ just }} cosmic-greeter/build-release
    {{ just }} cosmic-idle/build-release
    {{ just }} cosmic-initial-setup/build-release
    {{ just }} cosmic-launcher/build-release
    {{ just }} cosmic-monitor/build-release
    {{ just }} cosmic-notifications/build-release
    {{ just }} cosmic-osd/build-release
    {{ just }} cosmic-panel/build-release
    {{ just }} cosmic-player/build-release
    {{ just }} cosmic-randr/build-release
    {{ just }} cosmic-screenshot/build-release
    {{ just }} cosmic-settings/build-release
    {{ make }} -C cosmic-settings-daemon all
    {{ just }} cosmic-session/build-release
    {{ just }} cosmic-store/build-release
    {{ just }} cosmic-term/build-release
    {{ make }} -C cosmic-wallpapers all
    {{ make }} -C cosmic-workspaces-epoch all
    {{ just }} pop-launcher/build-release
    {{ make }} -C xdg-desktop-portal-cosmic all

install rootdir="" prefix="/usr/local": build
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-applets/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-applibrary/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-bg/install
    {{ make }} -C cosmic-comp install DESTDIR={{rootdir}} prefix={{prefix}}
    install -Dm0755 cosmic-conf/target/release/cosmic-conf {{rootdir}}{{prefix}}/bin/cosmic-conf
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-edit/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-files/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-greeter/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-icons/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-idle/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-initial-setup/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-launcher/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-monitor/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-notifications/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-osd/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-panel/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-player/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-randr/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-screenshot/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-settings/install
    {{ make }} -C cosmic-settings-daemon install DESTDIR={{rootdir}} prefix={{prefix}}
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-session/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-store/install
    {{ just }} rootdir={{rootdir}} prefix={{prefix}} cosmic-term/install
    {{ make }} -C cosmic-wallpapers install DESTDIR={{rootdir}} prefix={{prefix}}
    {{ make }} -C cosmic-workspaces-epoch install DESTDIR={{rootdir}} prefix={{prefix}}
    {{ just }} rootdir={{rootdir}} pop-launcher/install
    {{ make }} -C xdg-desktop-portal-cosmic install DESTDIR={{rootdir}} prefix={{prefix}}
    # The waybar and rofi assets, and the power menu. Last, because it is the
    # only step that prints a warning worth reading: several of these files name
    # /usr/share/hyprcosmic as a literal -- a .rasi has no variables and the
    # autostart file is not a shell -- so a prefix other than /usr installs them
    # somewhere they will not be looked for. The script says which files.
    #
    # --no-session because cosmic-session/install above already placed
    # start-hyprcosmic and hyprcosmic.desktop, and installing them twice would
    # only make it unclear which recipe owns them.
    PREFIX={{prefix}} DESTDIR={{rootdir}} ./tools/install-assets.sh --no-session

_mkdir dir:
   mkdir -p dir

sysext dir=(invocation_directory() / "cosmic-sysext") version=("nightly-" + `git rev-parse --short HEAD`): (_mkdir dir) (install dir "/usr")
    #!/usr/bin/env sh
    mkdir -p {{dir}}/usr/lib/extension-release.d/
    cat >{{dir}}/usr/lib/extension-release.d/extension-release.cosmic-sysext <<EOF
    NAME="Cosmic DE"
    VERSION={{version}}
    $(cat /etc/os-release | grep '^ID=')
    $(cat /etc/os-release | grep '^VERSION_ID=')
    EOF
    echo "Done"

clean:
    rm -rf cosmic-sysext
    rm -rf cosmic-applets/target
    rm -rf cosmic-applibrary/target
    rm -rf cosmic-bg/target
    rm -rf cosmic-comp/target
    rm -rf cosmic-conf/target
    rm -rf cosmic-edit/target
    {{ just }} cosmic-files/clean
    rm -rf cosmic-greeter/target
    {{ just }} cosmic-idle/clean
    {{ just }} cosmic-initial-setup/clean
    rm -rf cosmic-launcher/target
    {{ just }} cosmic-monitor/clean
    rm -rf cosmic-panel/target
    rm -rf cosmic-player/target
    rm -rf cosmic-notifications/target
    rm -rf cosmic-osd/target
    rm -rf cosmic-randr/target
    rm -rf cosmic-screenshot/target
    rm -rf cosmic-settings/target
    rm -rf cosmic-settings-daemon/target
    rm -rf cosmic-session/target
    {{ just }} cosmic-store/clean
    {{ just }} cosmic-term/clean
    {{ make }} -C cosmic-wallpapers clean
    rm -rf cosmic-workspaces-epoch/target
    {{ just }} pop-launcher/clean
    rm -rf xdg-desktop-portal-cosmic/target
