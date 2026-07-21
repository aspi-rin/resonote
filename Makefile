.PHONY: dmg

DMG_DIR := src-tauri/target/release/bundle/dmg
BUNDLE_ID := app.resonote.desktop

dmg:
	CI=true npm run tauri build -- --bundles dmg
	tccutil reset Microphone $(BUNDLE_ID)
	tccutil reset ScreenCapture $(BUNDLE_ID)
	@dmg="$$(find "$(DMG_DIR)" -maxdepth 1 -type f -name '*.dmg' -exec stat -f '%m %N' {} \; | sort -nr | sed -n '1s/^[0-9]* //p')"; \
	if [ -z "$$dmg" ]; then \
		echo "No DMG found in $(DMG_DIR)" >&2; \
		exit 1; \
	fi; \
	echo "Opening $$dmg"; \
	open "$$dmg"
