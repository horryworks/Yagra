# Written by Yagra (yagra-core oxidized-config). Edits here are replaced on the next start.
#
# Pushes oxidized's Git repository to the destination Yagra holds (ADR-210 stage 4). Run by the
# `yagra-push` hook after every fetch that succeeded. Core decides everything: this asks whether the
# current head needs pushing, pushes it once with what core hands back, and reports how it went.
# git's output never leaves the container; only the failure kind core's patterns name does.
#
# What is pushed is not oxidized's own tree, whose files are named by node id: core hands a map from
# each id to a file name (`<name>.txt`, `deleted/<name>.txt` once the node is gone), and this builds
# a tree with those names from the same blobs - nothing is copied - as a commit on its own line of
# history, `refs/yagra/export`. oxidized's repository is never changed, so the version list Yagra
# shows keeps reading it as before.
#
# That tree goes under one folder at the destination (core's `folder`, ADR-210 stage 9). Before
# pushing, this fetches the destination's branch into `refs/yagra/remote` and builds the commit on
# top of it: the folder is replaced whole and everything beside it - files a person keeps in the
# same repository - stays as the destination holds it. Nothing is ever forced.

require 'base64'
require 'json'
require 'net/http'
require 'open3'
require 'tmpdir'
require 'uri'

REPO = '/home/oxidized/.config/oxidized/configs.git'
EXPORT_REF = 'refs/yagra/export'
# Where the destination's branch is fetched to. Never pushed, never read by oxidized.
REMOTE_REF = 'refs/yagra/remote'
# The folder oxidized files every Yagra node under (`oxidized::GROUP`).
GROUP = 'yagra'
URL = URI(ENV.fetch('YAGRA_PUSH_URL'))
TOKEN_HEADER = 'x-yagra-oxidized-token'
TOKEN = ENV.fetch('YAGRA_TOKEN')
LOCK = '/tmp/yagra-push.lock'
# A push of the whole history can be slow the first time; the hook's own limit is 300 seconds, and
# the three steps together stay inside it.
ASK_SECONDS = '30'
FETCH_SECONDS = '90'
PUSH_SECONDS = '120'
FOLDER_NAME = /\A[A-Za-z0-9_][A-Za-z0-9_.-]*\z/
LOCK_SECONDS = 150
NODE_ID = /\A\h{8}-\h{4}-\h{4}-\h{4}-\h{12}\z/
IDENTITY = {
  'GIT_AUTHOR_NAME' => 'Yagra', 'GIT_AUTHOR_EMAIL' => 'oxidized@yagra.invalid',
  'GIT_COMMITTER_NAME' => 'Yagra', 'GIT_COMMITTER_EMAIL' => 'oxidized@yagra.invalid'
}.freeze
# How many file names a commit message lists before it says how many more there are.
MESSAGE_NAMES = 20

def http
  h = Net::HTTP.new(URL.host, URL.port)
  h.open_timeout = 5
  h.read_timeout = 20
  h
end

def report(form)
  req = Net::HTTP::Post.new(URL.request_uri, TOKEN_HEADER => TOKEN)
  req.set_form_data(form)
  http.request(req)
end

def git(*args, input: nil, env: {})
  out, status = Open3.capture2(env, 'git', '--git-dir', REPO, *args, stdin_data: input)
  raise "git #{args.first} failed" unless status.success?

  out.strip
end

# The tree of `head` with every node's file renamed by `files` (id => path).
def export_tree(head, files)
  root = []
  deleted = []
  seen = {}
  listing, status = Open3.capture2('git', '--git-dir', REPO, 'ls-tree', '-r', '-z', head)
  raise 'git ls-tree failed' unless status.success?

  listing.split("\0").each do |entry|
    meta, path = entry.split("\t", 2)
    mode, type, sha = meta.split(' ')
    next unless type == 'blob'

    dir, id = File.split(path)
    next unless dir == GROUP && id.match?(NODE_ID)

    target = files[id] || "deleted/#{id}.txt"
    # Core never hands two ids one file, case-folded as well; if it ever did, fail rather than drop
    # one device's file. Compared exactly here: Ruby's case folding is not Rust's, and a check that
    # folded differently could stop every push over a name core had already told apart.
    raise "two configurations map to #{target}" if seen[target]

    seen[target] = true
    folder, name = target.include?('/') ? target.split('/', 2) : [nil, target]
    line = "#{mode} blob #{sha}\t#{name}"
    folder == 'deleted' ? deleted << line : root << line
  end
  root << "040000 tree #{git('mktree', input: deleted.join("\n") + "\n")}\tdeleted" unless deleted.empty?
  git('mktree', input: root.join("\n") + (root.empty? ? '' : "\n"))
end

# What changed between two trees, as one line a person reads in the history.
def message(prev_tree, tree)
  return 'Back up device configurations' if prev_tree.nil?

  changes = git('diff-tree', '-r', '-M', '--name-status', prev_tree, tree).lines.map(&:chomp)
  words = changes.first(MESSAGE_NAMES).map do |line|
    kind, *paths = line.split("\t")
    case kind[0]
    when 'A' then "Add #{paths[0]}"
    when 'D' then "Remove #{paths[0]}"
    when 'R' then "Move #{paths[0]} to #{paths[1]}"
    else "Update #{paths[0]}"
    end
  end
  more = changes.size - words.size
  words << "and #{more} more" if more.positive?
  words.empty? ? 'Back up device configurations' : words.join(', ')
end

# One level of `tree` as name => [mode, type, id]; empty for nil. Read NUL-separated, because the
# destination's own files can be named anything.
def entries(tree)
  out = {}
  return out if tree.nil?

  listing, status = Open3.capture2('git', '--git-dir', REPO, 'ls-tree', '-z', tree)
  raise 'git ls-tree failed' unless status.success?

  listing.split("\0").each do |entry|
    meta, name = entry.split("\t", 2)
    mode, type, id = meta.split(' ')
    out[name] = [mode, type, id]
  end
  out
end

def mktree(level)
  git('mktree', '-z', input: level.map { |name, (mode, type, id)| "#{mode} #{type} #{id}\t#{name}\0" }.join)
end

# `tree` without what `old` holds unchanged: an entry equal in both is dropped, a folder in both is
# compared inside, and an emptied folder goes. Used once per destination, to clear the files the
# layout before the folder put at its top (stage 9 decision 77); a file a person has since edited is
# no longer equal and stays.
def without(tree, old)
  old_level = entries(old)
  kept = {}
  entries(tree).each do |name, entry|
    was = old_level[name]
    if was.nil?
      kept[name] = entry
    elsif was[2] == entry[2]
      next
    elsif was[1] == 'tree' && entry[1] == 'tree'
      inner = without(entry[2], was[2])
      kept[name] = [entry[0], 'tree', inner] if inner
    else
      kept[name] = entry
    end
  end
  kept.empty? ? nil : mktree(kept)
end

def tree_of(commit)
  git('rev-parse', "#{commit}^{tree}")
end

# The folder inside `commit`'s tree, or nil when it has none: a commit made before stage 9 put the
# files at the top.
def folder_in(commit, folder)
  entry = entries(tree_of(commit))[folder]
  entry && entry[1] == 'tree' ? entry[2] : nil
end

def ancestor?(older, newer)
  _, status = Open3.capture2('git', '--git-dir', REPO, 'merge-base', '--is-ancestor', older, newer)
  status.success?
end

# One push at a time: two hooks finishing together would race on the same refs. A later hook waits
# for the running one, so the newest head is always pushed by somebody.
lock = File.open(LOCK, File::RDWR | File::CREAT, 0o600)
waited = 0
until lock.flock(File::LOCK_EX | File::LOCK_NB)
  exit 0 if waited >= LOCK_SECONDS
  sleep 1
  waited += 1
end

head, status = Open3.capture2('git', '--git-dir', REPO, 'rev-parse', '--verify', 'HEAD')
exit 0 unless status.success?
head = head.strip

ask = Net::HTTP::Get.new("#{URL.request_uri}?head=#{head}", TOKEN_HEADER => TOKEN)
answer = http.request(ask)
# 204: nothing to push (no destination, already there, or waiting after a failure).
exit 0 unless answer.code == '200'
job = JSON.parse(answer.body)
base = { 'generation' => job['generation'].to_s, 'head' => head, 'layout' => job['layout'].to_s }

folder = job['folder'].to_s
unless folder.match?(FOLDER_NAME)
  report(base.merge('failure' => 'other'))
  exit 0
end

# The tree that goes under `folder`, and the export line's previous tip.
begin
  sub = export_tree(head, job['files'].to_h)
  prev, = Open3.capture2('git', '--git-dir', REPO, 'rev-parse', '-q', '--verify', "#{EXPORT_REF}^{commit}")
  prev = prev.strip
  prev = nil if prev.empty?
  # A tip from before stage 9 holds the files at the top of its tree, not under `folder`.
  prev_sub = prev && folder_in(prev, folder)
  old_layout = prev && prev_sub.nil? ? tree_of(prev) : nil
rescue StandardError
  report(base.merge('failure' => 'other'))
  exit 0
end

Dir.mktmpdir('yagra-push') do |dir|
  env = {
    'GIT_TERMINAL_PROMPT' => '0',
    'GIT_ASKPASS' => '/bin/false',
    'SSH_ASKPASS' => '/bin/false',
    'HOME' => dir,
    'GIT_CONFIG_NOSYSTEM' => '1'
  }
  known_hosts = File.join(dir, 'known_hosts')
  pinned = job['known_hosts'].to_s
  if job['auth'] == 'ssh_key'
    key = File.join(dir, 'key')
    File.write(key, job['private_key'], perm: 0o600)
    File.write(known_hosts, pinned, perm: 0o600)
    strict = pinned.empty? ? 'accept-new' : 'yes'
    env['GIT_SSH_COMMAND'] = "ssh -F /dev/null -i #{key} -o IdentitiesOnly=yes -o BatchMode=yes " \
                             "-o UserKnownHostsFile=#{known_hosts} -o StrictHostKeyChecking=#{strict} " \
                             '-o HashKnownHosts=no -o ConnectTimeout=15'
  else
    # In the environment, not on the command line, where `ps` in the container would show it.
    basic = Base64.strict_encode64("#{job['username']}:#{job['token']}")
    env['GIT_CONFIG_COUNT'] = '1'
    env['GIT_CONFIG_KEY_0'] = 'http.extraHeader'
    env['GIT_CONFIG_VALUE_0'] = "Authorization: Basic #{basic}"
  end

  # Every command that reaches the destination goes through here.
  # HTTP/1.1, never HTTP/2: the bundled image's libcurl-gnutls frees the transfer when an HTTP/2
  # server answers an upload with RST_STREAM (CVE-2026-92392), which a busy Git host sends with no
  # attacker involved. Upstream has no fixed image yet (ADR-215 increment 2, decision 4). Harmless
  # over SSH, which never reads it. No automatic gc: a fetch must not repack oxidized's repository.
  remote = lambda do |seconds, *args|
    Open3.capture2e(env, 'timeout', seconds, 'git', '-c', 'http.version=HTTP/1.1', '-c', 'gc.auto=0',
                    '--git-dir', REPO, *args)
  end
  kind_of = lambda do |output, status|
    lower = output.downcase
    hit = job['patterns'].find { |_, needle| lower.include?(needle) }
    hit ? hit[0] : (status.exitstatus == 124 ? 'unreachable' : 'other')
  end
  finish = lambda do |outcome|
    form = base.merge('failure' => '', 'commit' => '').merge(outcome)
    if job['auth'] == 'ssh_key' && pinned.empty? && File.exist?(known_hosts)
      text = File.read(known_hosts)
      form['known_hosts'] = text unless text.strip.empty?
    end
    report(form)
  end
  branch = "refs/heads/#{job['branch']}"

  # What the destination holds now (decision 76). An empty repository has no branch, and then
  # there is nothing to keep.
  output, status = remote.call(ASK_SECONDS, 'ls-remote', job['url'], branch)
  next finish.call('failure' => kind_of.call(output, status)) unless status.success?

  tip = nil
  if output.lines.any? { |line| line.split("\t", 2)[1].to_s.strip == branch }
    output, status = remote.call(FETCH_SECONDS, 'fetch', '--no-tags', job['url'], "+#{branch}:#{REMOTE_REF}")
    next finish.call('failure' => kind_of.call(output, status)) unless status.success?

    tip = git('rev-parse', '--verify', "#{REMOTE_REF}^{commit}")
  end

  # The commit to push (decisions 77 and 78): the destination's tree with `folder` replaced, on top
  # of both the export line and the destination's branch.
  begin
    tip_tree = tip && tree_of(tip)
    taken = entries(tip_tree)[folder]
    # A file a person named like the folder is theirs; replacing it is not Yagra's call (decision 79).
    next finish.call('failure' => 'rejected') if taken && taken[1] != 'tree'

    level = entries(old_layout ? without(tip_tree, old_layout) : tip_tree)
    level[folder] = ['040000', 'tree', sub]
    tree = mktree(level)

    parents = [prev, tip].compact.uniq
    if parents.size == 2
      if ancestor?(prev, tip)
        parents = [tip]
      elsif ancestor?(tip, prev)
        parents = [prev]
      end
    end
    if parents.size == 1 && tree_of(parents[0]) == tree
      commit = parents[0]
    else
      text = old_layout ? "Move device configurations into #{folder}/" : message(prev_sub, sub)
      commit = git('commit-tree', tree, *parents.flat_map { |p| ['-p', p] }, '-m', text, env: IDENTITY)
    end
    git('update-ref', EXPORT_REF, commit, *(prev ? [prev] : [])) if commit != prev
  rescue StandardError
    next finish.call('failure' => 'other')
  end

  output, status = remote.call(PUSH_SECONDS, 'push', '--porcelain', job['url'], "#{commit}:#{branch}")
  finish.call(status.success? ? { 'commit' => commit } : { 'failure' => kind_of.call(output, status) })
end
