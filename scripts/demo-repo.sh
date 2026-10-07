#!/usr/bin/env bash
# Builds a made-up repository with a lively history, for screenshots and demos: feature branches that run side by
# side, pull-request merges, a squash merge, a release branch, a hotfix, tags, a remote, and a change not committed.
# Nobody in it is real.
#
#   scripts/demo-repo.sh /tmp/acme-app
#
set -euo pipefail

DIR="${1:?usage: scripts/demo-repo.sh DIR}"
rm -rf "$DIR" "$DIR.remote.git"
mkdir -p "$DIR"
cd "$DIR"

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
git init -q -b main
git config commit.gpgsign false
git config core.autocrlf false

# The clock starts a while ago and moves on with every commit.
T=$(( $(date +%s) - 11 * 86400 ))

who() {
  case "$1" in
    mara)  NAME="Mara Chen";   MAIL="mara@acme.example" ;;
    tomas) NAME="Tomás Ruiz";  MAIL="tomas@acme.example" ;;
    priya) NAME="Priya Nair";  MAIL="priya@acme.example" ;;
    sam)   NAME="Sam Okoye";   MAIL="sam@acme.example" ;;
  esac
  export GIT_AUTHOR_NAME="$NAME" GIT_AUTHOR_EMAIL="$MAIL" GIT_COMMITTER_NAME="$NAME" GIT_COMMITTER_EMAIL="$MAIL"
}

stamp() { T=$(( T + $1 )); export GIT_AUTHOR_DATE="@$T +0000" GIT_COMMITTER_DATE="@$T +0000"; }

# commit WHO "message" FILE   (the file's new content is read from stdin)
commit() {
  who "$1"; stamp "${GAP:-9000}"
  mkdir -p "$(dirname "$3")"
  cat > "$3"
  git add -A
  git commit -q -m "$2"
}

# merge_pr WHO NUMBER BRANCH: a merge commit worded the way GitHub words it
merge_pr() {
  who "$1"; stamp 4200
  git merge -q --no-ff -m "Merge pull request #$2 from acme/$3" "$3"
}

# ---- main ---------------------------------------------------------------------------------------

commit mara "chore: scaffold the project" README.md <<'EOF'
# Acme

Booking and delivery for the Acme fleet.
EOF
commit mara "chore: app shell" src/app.ts <<'EOF'
import { login } from "./auth/login";

export function start(): void {
  console.log("acme starting");
}
EOF
commit tomas "feat: login screen" src/auth/login.ts <<'EOF'
export interface Session {
  token: string;
  expiresAt: number;
}

export async function login(email: string, password: string): Promise<Session> {
  const response = await fetch("/api/login", {
    method: "POST",
    body: JSON.stringify({ email, password }),
  });
  if (!response.ok) {
    throw new Error("login failed");
  }
  return response.json();
}
EOF
commit mara "docs: describe the release process" README.md <<'EOF'
# Acme

Booking and delivery for the Acme fleet.

## Releasing

Cut `release/x.y.z` from `develop`, fix only bugs on it, then merge it into `main` and tag it.
EOF
git tag v0.9.0

# ---- develop and the work that runs beside it -------------------------------------------------------

git checkout -q -b develop
commit mara "chore: set up continuous integration" .github/workflows/ci.yml <<'EOF'
name: ci
on: [push, pull_request]
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: npm ci && npm test
EOF

git checkout -q -b feature/74-driver-reporting
git checkout -q develop && git checkout -q -b feature/75-booking-calendar
git checkout -q develop && git checkout -q -b feature/81-vip-qr
git checkout -q develop && git checkout -q -b bugfix/90-login-crash

git checkout -q feature/74-driver-reporting
commit priya "feat: driver report data model" src/reports/driver.ts <<'EOF'
export interface DriverReport {
  driverId: string;
  week: string;
  trips: number;
  distanceKm: number;
}

export function emptyReport(driverId: string, week: string): DriverReport {
  return { driverId, week, trips: 0, distanceKm: 0 };
}
EOF

git checkout -q feature/75-booking-calendar
commit tomas "feat: booking calendar grid" src/booking/calendar.ts <<'EOF'
export type Slot = { start: Date; end: Date; free: boolean };

export function daySlots(day: Date, bookings: Slot[]): Slot[] {
  const slots: Slot[] = [];
  for (let hour = 8; hour < 18; hour++) {
    const start = new Date(day);
    start.setHours(hour, 0, 0, 0);
    const end = new Date(start);
    end.setHours(hour + 1);
    slots.push({ start, end, free: !bookings.some((b) => b.start < end && b.end > start) });
  }
  return slots;
}
EOF

git checkout -q bugfix/90-login-crash
commit tomas "fix: login crashed on an empty password" src/auth/login.ts <<'EOF'
export interface Session {
  token: string;
  expiresAt: number;
}

export async function login(email: string, password: string): Promise<Session> {
  if (!email || !password) {
    throw new Error("email and password are required");
  }
  const response = await fetch("/api/login", {
    method: "POST",
    body: JSON.stringify({ email, password }),
  });
  if (!response.ok) {
    throw new Error("login failed");
  }
  return response.json();
}
EOF

git checkout -q feature/81-vip-qr
commit sam "feat: generate a QR code per VIP pass" src/vip/qr.ts <<'EOF'
export function passPayload(passId: string, guest: string): string {
  return `acme:vip:${passId}:${encodeURIComponent(guest)}`;
}
EOF

git checkout -q feature/74-driver-reporting
commit priya "feat: weekly driver summary" src/reports/driver.ts <<'EOF'
export interface DriverReport {
  driverId: string;
  week: string;
  trips: number;
  distanceKm: number;
  averageKmPerTrip: number;
}

export function emptyReport(driverId: string, week: string): DriverReport {
  return { driverId, week, trips: 0, distanceKm: 0, averageKmPerTrip: 0 };
}

export function summarize(driverId: string, week: string, trips: number[]): DriverReport {
  const distanceKm = trips.reduce((sum, km) => sum + km, 0);
  const average = trips.length === 0 ? 0 : distanceKm / trips.length;
  return { driverId, week, trips: trips.length, distanceKm, averageKmPerTrip: Math.round(average * 10) / 10 };
}
EOF

git checkout -q feature/75-booking-calendar
commit tomas "feat: mark booked slots" src/booking/calendar.ts <<'EOF'
export type Slot = { start: Date; end: Date; free: boolean; who?: string };

export function daySlots(day: Date, bookings: Slot[]): Slot[] {
  const slots: Slot[] = [];
  for (let hour = 8; hour < 18; hour++) {
    const start = new Date(day);
    start.setHours(hour, 0, 0, 0);
    const end = new Date(start);
    end.setHours(hour + 1);
    const taken = bookings.find((b) => b.start < end && b.end > start);
    slots.push({ start, end, free: !taken, who: taken?.who });
  }
  return slots;
}
EOF

git checkout -q feature/81-vip-qr
commit sam "feat: show the QR code on the pass screen" src/vip/pass.ts <<'EOF'
import { passPayload } from "./qr";

export function renderPass(passId: string, guest: string): string {
  return `<img alt="VIP pass" data-qr="${passPayload(passId, guest)}">`;
}
EOF
commit sam "fix: encode guest names in the QR payload" src/vip/qr.ts <<'EOF'
export function passPayload(passId: string, guest: string): string {
  const name = encodeURIComponent(guest.trim());
  return `acme:vip:${passId}:${name}`;
}
EOF

git checkout -q feature/74-driver-reporting
commit priya "test: cover a week with no trips" src/reports/driver.test.ts <<'EOF'
import { summarize } from "./driver";

test("a week with no trips averages zero", () => {
  expect(summarize("d1", "2026-W40", []).averageKmPerTrip).toBe(0);
});
EOF

# Merge the finished work into develop, one pull request after another.
git checkout -q develop
merge_pr tomas 14 bugfix/90-login-crash
merge_pr priya 12 feature/74-driver-reporting
merge_pr mara 13 feature/75-booking-calendar

git checkout -q -b bugfix/91-empty-calendar
commit tomas "fix: an empty day showed no slots" src/booking/empty.ts <<'EOF'
export function emptyDay(): never[] {
  return [];
}
EOF
git checkout -q develop
merge_pr tomas 15 bugfix/91-empty-calendar

# The VIP work comes back squashed into one commit; its branch stays behind, which plain git never marks as merged.
who sam; stamp 5400
git merge -q --squash feature/81-vip-qr
git commit -q -m "feat: VIP passes with QR codes (#81)"

# ---- the release ---------------------------------------------------------------------------------

git checkout -q -b release/1.0.0
commit mara "chore: bump the version to 1.0.0" package.json <<'EOF'
{ "name": "acme", "version": "1.0.0" }
EOF
git checkout -q -b hotfix/92-null-token
commit tomas "fix: refresh failed when the token was null" src/auth/login.ts <<'EOF'
export interface Session {
  token: string | null;
  expiresAt: number;
}

export async function login(email: string, password: string): Promise<Session> {
  if (!email || !password) {
    throw new Error("email and password are required");
  }
  const response = await fetch("/api/login", {
    method: "POST",
    body: JSON.stringify({ email, password }),
  });
  if (!response.ok) {
    throw new Error("login failed");
  }
  return response.json();
}
EOF
git checkout -q release/1.0.0
merge_pr tomas 18 hotfix/92-null-token
git checkout -q main
merge_pr mara 19 release/1.0.0
git tag v1.0.0

# ---- after the release, on develop ---------------------------------------------------------------

git checkout -q develop
git merge -q --no-edit --no-ff -m "Merge branch 'release/1.0.0' into develop" release/1.0.0 >/dev/null 2>&1 || true

git checkout -q -b feature/95-dark-mode
commit mara "feat: dark colour scheme" src/theme.ts <<'EOF'
export const dark = { background: "#1e1e1e", text: "#d4d4d4", accent: "#4fc1ff" };
export const light = { background: "#ffffff", text: "#1e1e1e", accent: "#0b6bcb" };
EOF
git checkout -q develop && git checkout -q -b feature/88-offline-mode
commit priya "feat: queue bookings while offline" src/offline/sync.ts <<'EOF'
const queue: { id: string; payload: unknown }[] = [];

export function enqueue(id: string, payload: unknown): void {
  queue.push({ id, payload });
}

export function pending(): number {
  return queue.length;
}
EOF
git checkout -q feature/95-dark-mode
commit mara "feat: follow the system setting" src/theme.ts <<'EOF'
export const dark = { background: "#1e1e1e", text: "#d4d4d4", accent: "#4fc1ff" };
export const light = { background: "#ffffff", text: "#1e1e1e", accent: "#0b6bcb" };

export function scheme(): typeof dark {
  return window.matchMedia("(prefers-color-scheme: dark)").matches ? dark : light;
}
EOF
git checkout -q feature/88-offline-mode
commit priya "feat: send the queue when the network is back" src/offline/sync.ts <<'EOF'
const queue: { id: string; payload: unknown }[] = [];

export function enqueue(id: string, payload: unknown): void {
  queue.push({ id, payload });
}

export function pending(): number {
  return queue.length;
}

export async function flush(send: (id: string, payload: unknown) => Promise<void>): Promise<void> {
  while (queue.length > 0) {
    const next = queue[0];
    await send(next.id, next.payload);
    queue.shift();
  }
}
EOF
git checkout -q develop
merge_pr mara 20 feature/95-dark-mode
commit sam "docs: how the VIP passes work" docs/vip.md <<'EOF'
# VIP passes

A pass is a QR code. Scanning it at the gate marks the guest as arrived.
EOF

# ---- a remote, so branches show where else they live --------------------------------------------------

git init -q --bare "$DIR.remote.git"
git remote add origin "$DIR.remote.git"
git push -q -u origin --all
git push -q origin --tags
# Work that is done is deleted here and kept on the remote, as it is in a real team.
git branch -q -D feature/74-driver-reporting feature/75-booking-calendar bugfix/90-login-crash bugfix/91-empty-calendar hotfix/92-null-token feature/95-dark-mode

# One change that is not committed yet.
cat >> src/app.ts <<'EOF'

export function stop(): void {
  console.log("acme stopping");
}
EOF

echo "Made $DIR ($(git rev-list --all --count) commits)"
