#!/bin/sh
pid="$1"
staged="$2"
destination="$3"
backup="$4"
while kill -0 "$pid" 2>/dev/null; do sleep 0.1; done
if [ -e "$backup" ] || ! mv "$destination" "$backup"; then exit 1; fi
if mv "$staged" "$destination"; then
	case "$destination" in
		*.app) /usr/bin/open "$destination" && rm -rf "$backup" ;;
		*) "$destination" >/dev/null 2>&1 & rm -rf "$backup" ;;
	esac
else
	mv "$backup" "$destination"
	case "$destination" in
		*.app) /usr/bin/open "$destination" ;;
		*) "$destination" >/dev/null 2>&1 & ;;
	esac
fi
