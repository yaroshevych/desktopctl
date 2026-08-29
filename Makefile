DESKTOP_DIR   := src/desktop
APP_NAME      := DesktopCtl.app
DIST_DIR      := $(DESKTOP_DIR)/dist
APP_DIR       := $(DIST_DIR)/$(APP_NAME)
CONTENTS_DIR  := $(APP_DIR)/Contents
MACOS_DIR     := $(CONTENTS_DIR)/MacOS
CLI_PATH      := $(MACOS_DIR)/desktopctl
INSTALL_APP   := /Applications/$(APP_NAME)
SDK           := $(shell xcrun --show-sdk-path)

SWIFT_SOURCES := \
	$(DESKTOP_DIR)/app/ui-swift/Models.swift \
	$(DESKTOP_DIR)/app/ui-swift/DaemonIPC.swift \
	$(DESKTOP_DIR)/app/ui-swift/JournalDialog.swift \
	$(DESKTOP_DIR)/app/ui-swift/AppPolicyDialog.swift \
	$(DESKTOP_DIR)/app/ui-swift/SetupAccessDialog.swift \
	$(DESKTOP_DIR)/app/ui-swift/DesktopCtlSettings.swift \
	$(DESKTOP_DIR)/app/ui-swift/main.swift

.PHONY: install

install:
	cd $(DESKTOP_DIR) && cargo build --release -p desktopctld -p desktopctl
	rm -rf $(APP_DIR)
	mkdir -p $(MACOS_DIR) $(CONTENTS_DIR)/Resources $(DIST_DIR)
	cp $(DESKTOP_DIR)/target/release/desktopctld $(MACOS_DIR)/desktopctld
	cp $(DESKTOP_DIR)/target/release/desktopctl-cli $(CLI_PATH)
	ln -sfn ./DesktopCtl.app/Contents/MacOS/desktopctl $(DIST_DIR)/desktopctl
	cp $(DESKTOP_DIR)/daemon/packaging/macos/Info.plist $(CONTENTS_DIR)/Info.plist
	printf 'APPL????' > $(CONTENTS_DIR)/PkgInfo
	swift $(DESKTOP_DIR)/scripts/gen_icns.swift $(CONTENTS_DIR)/Resources/AppIcon.icns
	swiftc -O \
		-sdk $(SDK) \
		-target arm64-apple-macosx13.0 \
		-framework AppKit \
		-framework Foundation \
		-framework SwiftUI \
		$(SWIFT_SOURCES) \
		-o $(MACOS_DIR)/desktopctl-dialogs
	chmod +x $(MACOS_DIR)/desktopctld $(CLI_PATH) $(MACOS_DIR)/desktopctl-dialogs
	codesign --force --deep --options runtime --sign - $(APP_DIR)
	rm -rf $(INSTALL_APP)
	cp -R $(APP_DIR) $(INSTALL_APP)
