# Drives Yagra's config push script against a local destination repository, inside the pinned
# oxidized image (ADR-210 stage 9). Run by `scripts/oxidized-script-test.sh`; not shipped.
#
# The script fetches the destination and builds its commit on top of it, keeping everything outside
# the folder core names. That is git plumbing run by the image's own git and Ruby, so it is checked
# here rather than read: an empty destination, one a person added files to, one still holding the
# layout from before the folder, one whose top-level `yagra` is a file, and a second run with
# nothing new. The destination is a bare repository on a local path, which git pushes to the same
# way as to a server once the URL check (core's, not this script's) is out of the way.
#
# Usage: ruby push.rb <path to yagra-push.rb>

require 'json'
require 'socket'
require 'tmpdir'
require 'fileutils'
require 'open3'
require 'uri'

SCRIPT = ARGV.fetch(0)
# The repository the script reads; the composition names this path and so does the script.
OX_REPO = '/home/oxidized/.config/oxidized/configs.git'
TOKEN = 'T0ken'
CORE_PORT = 8098
NODE = '00000000-0000-4000-8000-000000000001'
GONE = '00000000-0000-4000-8000-000000000009'
IDENTITY = {
  'GIT_AUTHOR_NAME' => 'Someone', 'GIT_AUTHOR_EMAIL' => 'someone@example.com',
  'GIT_COMMITTER_NAME' => 'Someone', 'GIT_COMMITTER_EMAIL' => 'someone@example.com'
}.freeze
# A few of core's patterns, enough to name the failures these cases can produce.
PATTERNS = [%w[rejected [rejected]], %w[rejected non-fast-forward]].freeze

def git(repo, *args, input: nil)
  out, status = Open3.capture2(IDENTITY, 'git', '--git-dir', repo, *args, stdin_data: input)
  raise "git #{args.join(' ')} failed" unless status.success?

  out.strip
end

def blob(repo, text)
  git(repo, 'hash-object', '-w', '--stdin', input: text)
end

# A commit in `repo` holding `files` (path => text), on top of `parents`.
def commit(repo, files, parents: [], message: 'x')
  tree = build(repo, files)
  git(repo, 'commit-tree', tree, *parents.flat_map { |p| ['-p', p] }, '-m', message)
end

def build(repo, files)
  top = {}
  files.each do |path, text|
    head, rest = path.split('/', 2)
    if rest
      (top[head] ||= {})[rest] = text
    else
      top[head] = text
    end
  end
  lines = top.map do |name, value|
    if value.is_a?(Hash)
      "040000 tree #{build(repo, value)}\t#{name}"
    else
      "100644 blob #{blob(repo, value)}\t#{name}"
    end
  end
  git(repo, 'mktree', input: lines.join("\n") + (lines.empty? ? '' : "\n"))
end

# path => text of a commit's whole tree, each text without its last newline.
def files_of(repo, commit)
  listing = git(repo, 'ls-tree', '-r', '--name-only', commit)
  listing.lines.map(&:chomp).to_h { |path| [path, git(repo, 'show', "#{commit}:#{path}")] }
end

def parents_of(repo, commit)
  git(repo, 'rev-list', '--parents', '-n', '1', commit).split(' ').drop(1)
end

def tip(repo)
  out, status = Open3.capture2('git', '--git-dir', repo, 'rev-parse', '-q', '--verify', 'refs/heads/main')
  status.success? ? out.strip : nil
end

# The one route of core the script speaks to.
class FakeCore
  attr_reader :reports

  def initialize(job)
    @job = job
    @reports = []
  end

  def serve(client)
    request = client.gets.to_s
    headers = {}
    while (line = client.gets) && line != "\r\n"
      k, v = line.split(':', 2)
      headers[k.downcase] = v.to_s.strip
    end
    body = client.read(headers['content-length'].to_i) if headers['content-length']
    if headers['x-yagra-oxidized-token'] != TOKEN
      client.write("HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
    elsif request.start_with?('GET')
      text = @job.to_json
      client.write("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: #{text.bytesize}\r\nConnection: close\r\n\r\n#{text}")
    else
      @reports << URI.decode_www_form(body.to_s).to_h
      client.write("HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n")
    end
  ensure
    client.close rescue nil
  end
end

def listen(port, handler)
  server = TCPServer.new('127.0.0.1', port)
  Thread.new do
    loop do
      client = server.accept
      Thread.new(client) { |c| handler.serve(c) }
    end
  rescue IOError
    nil
  end
  server
end

# oxidized's repository with one version: the device's file and a deleted device's file, filed
# under oxidized's group folder by node id.
def fresh_oxidized(text = "hostname sw-01\n")
  FileUtils.rm_rf(OX_REPO)
  FileUtils.mkdir_p(File.dirname(OX_REPO))
  system('git', 'init', '--bare', '-q', OX_REPO, exception: true)
  new_version(text)
end

def new_version(text)
  head = commit(OX_REPO, { "yagra/#{NODE}" => text, "yagra/#{GONE}" => "hostname sw-09\n" },
                parents: [tip_of_ox].compact)
  git(OX_REPO, 'update-ref', 'HEAD', head)
end

def tip_of_ox
  out, status = Open3.capture2('git', '--git-dir', OX_REPO, 'rev-parse', '-q', '--verify', 'HEAD')
  status.success? ? out.strip : nil
end

def tip_of_export
  out, status = Open3.capture2('git', '--git-dir', OX_REPO, 'rev-parse', '-q', '--verify', 'refs/yagra/export')
  status.success? ? out.strip : nil
end

def fresh_destination
  dir = Dir.mktmpdir('dest')
  system('git', 'init', '--bare', '-q', dir, exception: true)
  dir
end

def push(dest)
  core = FakeCore.new(
    'generation' => 1, 'url' => dest, 'branch' => 'main', 'auth' => 'https_token',
    'username' => 'x', 'token' => 'y', 'known_hosts' => '', 'patterns' => PATTERNS,
    'files' => [[NODE, 'sw-01.txt']], 'folder' => 'yagra', 'layout' => 'L'
  )
  server = listen(CORE_PORT, core)
  env = {
    'YAGRA_TOKEN' => TOKEN,
    'YAGRA_PUSH_URL' => "http://127.0.0.1:#{CORE_PORT}/internal/oxidized/push"
  }
  # The environment the hook gives, and nothing else (`unsetenv_others`, as oxidized spawns it).
  pid = Process.spawn(env, 'ruby', SCRIPT, unsetenv_others: true)
  waited = 0
  waited += 1 while Process.waitpid(pid, Process::WNOHANG).nil? && sleep(1) && waited < 120
  Process.kill('KILL', pid) if waited >= 120
  server.close
  core.reports.last || {}
end

$failures = 0
def check(name, ok, detail = nil)
  puts "#{ok ? 'PASS' : 'FAIL'} #{name}#{ok || detail.nil? ? '' : " - #{detail}"}"
  $failures += 1 unless ok
end

# 1. An empty destination gets the folder and nothing else.
fresh_oxidized
dest = fresh_destination
report = push(dest)
files = tip(dest) ? files_of(dest, tip(dest)) : {}
check('empty destination: pushed', report['failure'] == '' && report['commit'] == tip(dest), report)
check('empty destination: only the folder',
      files.keys.sort == ['yagra/deleted/' + GONE + '.txt', 'yagra/sw-01.txt'], files.keys)

# 2. A person adds a file beside the folder; the next version keeps it and fast-forwards.
mine = commit(dest, files.merge('manual/fw-01.txt' => "hostname fw-01\n"), parents: [tip(dest)])
git(dest, 'update-ref', 'refs/heads/main', mine)
new_version("hostname sw-01\nntp server 192.0.2.1\n")
report = push(dest)
files = files_of(dest, tip(dest))
check('beside the folder: pushed', report['failure'] == '', report)
check('beside the folder: the person\'s file stays', files['manual/fw-01.txt'] == 'hostname fw-01', files.keys)
check('beside the folder: the new version is there', files['yagra/sw-01.txt'].include?('ntp server'))
check('beside the folder: one parent, the person\'s commit', parents_of(dest, tip(dest)) == [mine])

# 3. A second run with nothing new adds no commit.
before = tip(dest)
report = push(dest)
check('nothing new: no commit', report['failure'] == '' && tip(dest) == before, report)

# 4. A destination with history of its own, unrelated to Yagra's: kept, two parents.
fresh_oxidized
dest = fresh_destination
theirs = commit(dest, { 'README' => "configs\n", 'manual/fw-01.txt' => "hostname fw-01\n" })
git(dest, 'update-ref', 'refs/heads/main', theirs)
report = push(dest) # first: no export line yet, so one parent (theirs)
check('unrelated history: pushed', report['failure'] == '', report)
check('unrelated history: their files stay', files_of(dest, tip(dest)).key?('README'))
check('unrelated history: built on theirs', parents_of(dest, tip(dest)) == [theirs])

# 5. The layout from before the folder: Yagra's files leave the top, a file a person edited stays.
fresh_oxidized
dest = fresh_destination
old = commit(OX_REPO, { 'sw-01.txt' => "hostname sw-01\n", 'sw-02.txt' => "hostname sw-02\n",
                        "deleted/#{GONE}.txt" => "hostname sw-09\n" }, message: 'old layout')
git(OX_REPO, 'update-ref', 'refs/yagra/export', old)
# What the destination received from the old script: the export line itself.
system('git', '--git-dir', dest, 'fetch', '-q', OX_REPO, "refs/yagra/export:refs/heads/main", exception: true)
edited = commit(dest, { 'sw-01.txt' => "hostname sw-01\n", 'sw-02.txt' => "hostname sw-02 edited\n",
                        "deleted/#{GONE}.txt" => "hostname sw-09\n" }, parents: [old])
git(dest, 'update-ref', 'refs/heads/main', edited)
report = push(dest)
files = files_of(dest, tip(dest))
check('old layout: pushed', report['failure'] == '', report)
check('old layout: the untouched top-level files leave',
      !files.key?('sw-01.txt') && !files.key?("deleted/#{GONE}.txt"), files.keys)
check('old layout: the edited file stays', files['sw-02.txt'] == 'hostname sw-02 edited', files.keys)
check('old layout: the files are under the folder', files.key?('yagra/sw-01.txt'), files.keys)
check('old layout: says it moved', git(dest, 'log', '-1', '--format=%s', tip(dest)) == 'Move device configurations into yagra/')

# 6. A file named like the folder is not replaced.
fresh_oxidized
dest = fresh_destination
theirs = commit(dest, { 'yagra' => "not a folder\n" })
git(dest, 'update-ref', 'refs/heads/main', theirs)
report = push(dest)
check('folder taken by a file: rejected', report['failure'] == 'rejected', report)
check('folder taken by a file: destination unchanged', tip(dest) == theirs)

# 7. The layout from before the folder, and the first push under it refused: the next run must still
#    clear the old top-level files (the export line moves only once a push succeeds).
fresh_oxidized
dest = fresh_destination
old = commit(OX_REPO, { 'sw-01.txt' => "hostname sw-01\n", "deleted/#{GONE}.txt" => "hostname sw-09\n" },
             message: 'old layout')
git(OX_REPO, 'update-ref', 'refs/yagra/export', old)
system('git', '--git-dir', dest, 'fetch', '-q', OX_REPO, "refs/yagra/export:refs/heads/main", exception: true)
hook = File.join(dest, 'hooks', 'pre-receive')
FileUtils.mkdir_p(File.dirname(hook))
File.write(hook, "#!/bin/sh\necho 'refused by a hook' >&2\nexit 1\n")
File.chmod(0o755, hook)
report = push(dest)
check('refused once: reported', report['failure'] != '', report)
check('refused once: destination unchanged', tip(dest) == old)
check('refused once: the export line did not move', tip_of_export == old)
File.delete(hook)
report = push(dest)
files = files_of(dest, tip(dest))
check('after the refusal: pushed', report['failure'] == '', report)
check('after the refusal: the old top-level files leave',
      !files.key?('sw-01.txt') && !files.key?("deleted/#{GONE}.txt"), files.keys)
check('after the refusal: the files are under the folder', files.key?('yagra/sw-01.txt'), files.keys)

exit($failures.zero? ? 0 : 1)
