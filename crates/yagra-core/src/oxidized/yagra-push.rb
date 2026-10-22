# Written by Yagra (yagra-core oxidized-config). Edits here are replaced on the next start.
#
# Pushes oxidized's Git repository to the destination Yagra holds (ADR-210 stage 4). Run by the
# `yagra-push` hook after every fetch that succeeded. Core decides everything: this asks whether the
# current head needs pushing, pushes it once with what core hands back, and reports how it went.
# git's output never leaves the container; only the failure kind core's patterns name does.

require 'base64'
require 'json'
require 'net/http'
require 'open3'
require 'tmpdir'
require 'uri'

REPO = '/home/oxidized/.config/oxidized/configs.git'
URL = URI(ENV.fetch('YAGRA_PUSH_URL'))
TOKEN_HEADER = 'x-yagra-oxidized-token'
TOKEN = ENV.fetch('YAGRA_TOKEN')
LOCK = '/tmp/yagra-push.lock'
# A push of the whole history can be slow the first time; the hook's own limit is 180 seconds.
PUSH_SECONDS = '120'
LOCK_SECONDS = 150

def http
  h = Net::HTTP.new(URL.host, URL.port)
  h.open_timeout = 5
  h.read_timeout = 20
  h
end

def report(job, head, failure, learned)
  form = { 'generation' => job['generation'].to_s, 'head' => head, 'failure' => failure.to_s }
  form['known_hosts'] = learned if learned
  req = Net::HTTP::Post.new(URL.request_uri, TOKEN_HEADER => TOKEN)
  req.set_form_data(form)
  http.request(req)
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
  output, status = Open3.capture2e(env, 'timeout', PUSH_SECONDS, 'git', '--git-dir', REPO, 'push',
                                   '--porcelain', job['url'], "HEAD:refs/heads/#{job['branch']}")
  failure = nil
  unless status.success?
    lower = output.downcase
    hit = job['patterns'].find { |_, needle| lower.include?(needle) }
    failure = hit ? hit[0] : (status.exitstatus == 124 ? 'unreachable' : 'other')
  end
  learned = nil
  if job['auth'] == 'ssh_key' && pinned.empty? && File.exist?(known_hosts)
    text = File.read(known_hosts)
    learned = text unless text.strip.empty?
  end
  report(job, head, failure, learned)
end
