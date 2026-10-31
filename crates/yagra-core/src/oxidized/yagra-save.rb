# Written by Yagra (yagra-core oxidized-config). Edits here are replaced on the next start.
#
# Saves a device's running configuration, or checks whether it is saved, or both: with automatic
# save on, it compares and saves in the same login when the two differ (ADR-222). Run by the
# `yagra-save` hook after every fetch that succeeded. Core decides everything: this asks what to do
# for the node just fetched, and for almost every node the answer is nothing and the script ends
# before oxidized is even loaded. When there is work, core hands back the login the fetch used and
# the commands for the device's model, taken from its fixed table - this script has no command of
# its own and sends nothing else. It reports a kind of outcome, never what the device printed,
# except the startup configuration it was asked to keep, filtered by oxidized's secret rules.
#
# oxidized's own Node, Input and Model classes do the logging in, so prompts, paging and enable
# work exactly as in the fetch that just succeeded. They are oxidized's internals, not a published
# API: Yagra's script test runs this file in the pinned oxidized image, so an upgrade that moves
# them fails there first.

require 'json'
require 'net/http'
require 'timeout'
require 'uri'

URL = URI(ENV.fetch('YAGRA_SAVE_URL'))
TOKEN_HEADER = 'x-yagra-oxidized-token'
TOKEN = ENV.fetch('YAGRA_TOKEN')
NODE = ENV.fetch('OX_NODE_NAME', '')
NODE_ID = /\A\h{8}-\h{4}-\h{4}-\h{4}-\h{12}\z/
# How many questions a save may ask before it is given up as unanswered.
MAX_QUESTIONS = 3

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
rescue StandardError
  nil
end

exit 0 unless NODE.match?(NODE_ID)

ask = Net::HTTP::Get.new("#{URL.request_uri}?node=#{NODE}", TOKEN_HEADER => TOKEN)
answer = begin
  http.request(ask)
rescue StandardError
  exit 0
end
# 204: nothing to do for this node.
exit 0 unless answer.code == '200'
job = JSON.parse(answer.body)
cmds = job['commands']
base = { 'node' => NODE, 'action' => job['action'].to_s, 'save' => job['save'].to_s }
# oxidized starts a hook with an empty environment; the hook command names the home. Without it
# oxidized would load its defaults, write them to a home of its own and log in as nobody.
unless ENV['OXIDIZED_HOME']
  report(base.merge('failure' => 'other'))
  exit 0
end

# What a connection failure is, by oxidized's and the transport's error class.
def failure_of(error)
  name = error.class.name.to_s
  return 'auth' if name.include?('AuthenticationFailed')
  return 'unreachable' if error.is_a?(SystemCallError) || name =~ /Timeout|Connection|Unreachable|Refused/

  'other'
end

begin
  require 'oxidized'
  Oxidized::Config.load({})
  # No `Oxidized::Logger.setup`: it creates `logs/` under OXIDIZED_HOME, which is mounted
  # read-only in oxidized's container, and nothing here needs oxidized's log.
  Oxidized.mgr = Oxidized::Manager.new
  Oxidized.hooks = Oxidized::HookManager.new
  vars = { 'enable' => job['enable'], 'ssh_port' => job['ssh_port'], 'telnet_port' => job['telnet_port'] }.compact
  node = Oxidized::Node.new(
    name: NODE, ip: job['ip'], model: job['model'], input: job['input'], group: job['group'],
    username: job['username'], password: job['password'], vars: vars
  )
  input = nil
  failure = nil
  node.input.each do |klass|
    node.model.input = klass.new
    node.model.input.connect(node)
    input = node.model.input
    break
  rescue StandardError, Timeout::Error => e
    failure = failure_of(e)
  end
  unless input&.connected?
    report(base.merge('failure' => failure || 'unreachable'))
    exit 0
  end
  input.connect_cli if input.respond_to?(:connect_cli)
  prompt = node.prompt
rescue StandardError, Timeout::Error => e
  report(base.merge('failure' => failure_of(e)))
  exit 0
end

# Whether running and startup differ, or nil when the model cannot say.
def unsaved?(input, cmds)
  return nil unless cmds['compare']

  out = input.cmd(cmds['compare']).to_s
  !out.match?(Regexp.new(cmds['same']))
end

form = base.dup
# Whether a save command is about to be or has been sent; once it has, an error must not read as a
# plain check.
saving = job['action'] == 'save'
begin
  before = unsaved?(input, cmds)
  form['unsaved_before'] = before.to_s
  saving ||= job['action'] == 'auto' && before == true
  if saving
    # The startup configuration about to be replaced, through the model so its secret filter runs.
    form['startup'] = node.model.cmd(cmds['startup']).to_s
    confirms = cmds['confirms'].map { |q, a| [Regexp.new(q), a] }
    expect = Regexp.union(prompt, *confirms.map(&:first))
    out = input.cmd(cmds['save'], expect).to_s
    seen = out.dup
    MAX_QUESTIONS.times do
      hit = confirms.find { |q, _| out.match?(q) }
      break unless hit

      out = input.cmd(hit[1], expect).to_s
      seen << out
    end
    form['failure'] = if seen.match?(Regexp.new(cmds['denied']))
                        'denied'
                      elsif seen.match?(Regexp.new(cmds['success']))
                        ''
                      else
                        'no_answer'
                      end
    form['unsaved_after'] = unsaved?(input, cmds).to_s if form['failure'].empty?
  end
rescue StandardError, Timeout::Error
  # A save whose answer never came back is the one outcome that must not read as saved.
  form['failure'] = saving ? 'no_answer' : 'other'
ensure
  begin
    input.disconnect_cli if input.respond_to?(:disconnect_cli)
  rescue StandardError
    nil
  end
end

report(form)
