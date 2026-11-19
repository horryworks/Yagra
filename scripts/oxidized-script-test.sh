#!/usr/bin/env bash
# Runs the config push (ADR-210 stage 9) and save (ADR-222) scripts inside the oxidized image the
# composition pins: the push against local destination repositories, the save against a fake
# device. The image is read from docker-compose.deploy.yml, so moving the pin is what makes
# this test the new version - the script leans on oxidized internals an upgrade can move.
#
# Needs Docker. Prints PASS/FAIL per case and exits non-zero on any failure.
set -euo pipefail
cd "$(dirname "$0")/.."
IMAGE=$(awk '$1 == "image:" && $2 ~ /^oxidized\/oxidized:/ { print $2; exit }' docker-compose.deploy.yml)
if [ -z "$IMAGE" ]; then
  echo "no oxidized image in docker-compose.deploy.yml" >&2
  exit 2
fi
echo "oxidized image: $IMAGE"
# The push script (ADR-210 stage 9) against local destination repositories.
docker run --rm --entrypoint ruby \
  -v "$PWD/crates/yagra-core/src/oxidized/yagra-push.rb:/t/yagra-push.rb:ro" \
  -v "$PWD/scripts/oxidized-script-test:/t/harness:ro" \
  "$IMAGE" /t/harness/push.rb /t/yagra-push.rb
docker run --rm --entrypoint ruby \
  -v "$PWD/crates/yagra-core/src/oxidized/yagra-save.rb:/t/yagra-save.rb:ro" \
  -v "$PWD/crates/yagra-core/src/oxidized/yagra-enable.rb:/t/yagra-enable.rb:ro" \
  -v "$PWD/scripts/oxidized-script-test:/t/harness:ro" \
  "$IMAGE" /t/harness/run.rb /t/yagra-save.rb /t/yagra-enable.rb
