# Drives Yagra's config save script against a fake device, inside the pinned oxidized image
# (ADR-222 decision 10). Run by `scripts/oxidized-script-test.sh`; not shipped.
#
# The script logs in with oxidized's own Node, Input and Model classes, which are oxidized's
# internals rather than a published API. Running it here, in the very image the composition pins,
# is what notices when an oxidized upgrade moves them. The fake device speaks like a Huawei VRP
# over telnet: a login, a `<sw-01>` prompt, `compare configuration`, `display saved-configuration`
# and a `save` that asks `[Y/N]:`. The fake core answers the script's question with the commands
# in `commands.json`, which a Rust test generates from `config_save::SAVE_COMMANDS`.
#
# Usage: ruby run.rb <path to yagra-save.rb>

require 'json'
require 'socket'
require 'tmpdir'
require 'uri'

SCRIPT = ARGV.fetch(0)
COMMANDS = JSON.parse(File.read(File.join(__dir__, 'commands.json')))
VRP = COMMANDS.find { |c| c['model'] == 'vrp' } or abort 'no vrp row in commands.json'
NODE = '00000000-0000-4000-8000-000000000222'
TOKEN = 'T0ken'
DEVICE_PORT = 2323
CORE_PORT = 8099
PROMPT = '<sw-01>'

# A device that speaks like VRP. `mode` decides how it answers a save, and `:already_saved` starts
# it with nothing to save.
class FakeDevice
  attr_reader :heard

  def initialize(mode)
    @mode = mode
    @heard = []
    @saved = mode == :already_saved
  end

  def serve(client)
    client.write("\r\nUsername:")
    client.gets
    client.write("\r\nPassword:")
    password = client.gets.to_s.strip
    if password != 'right'
      # VRP asks again; oxidized waits for a prompt that never comes.
      client.write("\r\nError: Authentication fail\r\nUsername:")
      sleep 30
      return
    end
    client.write("\r\nInfo: logged in\r\n#{PROMPT}")
    while (line = client.gets)
      cmd = line.strip
      @heard << cmd
      case cmd
      when 'screen-length 0 temporary'
        client.write("\r\nInfo: The configuration takes effect on the current user terminal.\r\n#{PROMPT}")
      when 'compare configuration'
        text = if @saved
                 'Info: The current configuration is the same as the next startup configuration.'
               else
                 "Warning: The current configuration is not the same as the next startup configuration file.\r\n+ sysname sw-01-new"
               end
        client.write("\r\n#{text}\r\n#{PROMPT}")
      when 'display saved-configuration'
        client.write("\r\n#\r\nsysname sw-01\r\n#\r\nreturn\r\n#{PROMPT}")
      when 'save'
        if @mode == :denied
          client.write("\r\nError: Unrecognized command found at '^' position.\r\n#{PROMPT}")
        else
          client.write("\r\nWarning: The current configuration will be written to the device. Continue? [Y/N]:")
        end
      when 'y'
        if @mode == :silent
          sleep 30
        else
          @saved = true
          client.write("\r\nInfo: Save the configuration successfully.\r\n#{PROMPT}")
        end
      when 'quit'
        break
      else
        client.write("\r\nError: Unrecognized command found at '^' position.\r\n#{PROMPT}")
      end
    end
  ensure
    client.close rescue nil
  end
end

# The two routes of core the script speaks to.
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
    nil # closed by the case that opened it
  end
  server
end

home = Dir.mktmpdir('ox')
File.write(File.join(home, 'config'), <<~YAML)
  ---
  username: unused
  password: unused
  model: vrp
  resolve_dns: false
  interval: 0
  retries: 0
  timeout: 5
  debug: false
  vars:
    remove_secret: true
  input:
    default: telnet
    debug: false
  output:
    default: file
    file:
      directory: #{home}/configs
  source:
    default: csv
    csv:
      file: #{home}/router.db
      delimiter: ":"
      map:
        name: 0
YAML
File.write(File.join(home, 'router.db'), '')

def job(action, password)
  {
    'action' => action, 'save' => (action == 'save' ? '11111111-1111-4111-8111-111111111111' : nil),
    'ip' => '127.0.0.1', 'model' => 'vrp', 'input' => 'telnet', 'group' => 'yagra',
    'username' => 'admin', 'password' => password, 'telnet_port' => DEVICE_PORT,
    'commands' => VRP
  }.compact
end

def run(home, name, mode, action: 'save', password: 'right')
  device = FakeDevice.new(mode)
  core = FakeCore.new(job(action, password))
  servers = [listen(DEVICE_PORT, device), listen(CORE_PORT, core)]
  env = {
    'OXIDIZED_HOME' => home, 'OX_NODE_NAME' => NODE, 'YAGRA_TOKEN' => TOKEN,
    'YAGRA_SAVE_URL' => "http://127.0.0.1:#{CORE_PORT}/internal/oxidized/save"
  }
  # The environment the hook gives, and nothing else: oxidized spawns hooks with
  # `unsetenv_others`, and a script that leaned on an inherited variable passed here before.
  pid = Process.spawn(env, 'ruby', SCRIPT, out: File::NULL, err: File::NULL, unsetenv_others: true)
  waited = 0
  waited += 1 while Process.waitpid(pid, Process::WNOHANG).nil? && sleep(1) && waited < 60
  Process.kill('KILL', pid) if waited >= 60
  servers.each(&:close)
  [core.reports.last || {}, device.heard]
end

failures = []
check = lambda do |name, ok, detail|
  puts "#{ok ? 'PASS' : 'FAIL'} #{name}#{ok ? '' : " - #{detail}"}"
  failures << name unless ok
end

r, heard = run(home, 'saved', :ok)
check.call('a save that works is reported as saved', r['failure'] == '' && r['action'] == 'save', r)
check.call('the save is sent, then the answer, in that order',
           heard.index('save') && heard.index('y') && heard.index('save') < heard.index('y'), heard)
check.call('the startup it replaced comes back', r['startup'].to_s.include?('sysname sw-01'), r)
check.call('unsaved before, saved after', r['unsaved_before'] == 'true' && r['unsaved_after'] == 'false', r)

r, heard = run(home, 'denied', :denied)
check.call('a refused save is reported as denied', r['failure'] == 'denied', r)
check.call('a refused save is never confirmed', !heard.include?('y'), heard)

r, = run(home, 'silent', :silent)
check.call('a save the device never confirms is not reported as saved', r['failure'] == 'no_answer', r)

r, heard = run(home, 'wrong password', :ok, password: 'wrong')
check.call('a refused login reports a failure', !r['failure'].to_s.empty?, r)
check.call('a refused login sends no save', !heard.include?('save'), heard)

r, heard = run(home, 'check', :ok, action: 'check')
check.call('a check reports what it saw', r['action'] == 'check' && r['unsaved_before'] == 'true', r)
check.call('a check sends no save', !heard.include?('save'), heard)

r, heard = run(home, 'auto, unsaved', :ok, action: 'auto')
check.call('automatic save of an unsaved device saves in the same login',
           r['action'] == 'auto' && r['failure'] == '' && r['unsaved_before'] == 'true' &&
             r['unsaved_after'] == 'false', r)
check.call('automatic save compares before it saves',
           heard.index('compare configuration') && heard.index('save') &&
             heard.index('compare configuration') < heard.index('save'), heard)
check.call('automatic save keeps the startup it replaced', r['startup'].to_s.include?('sysname sw-01'), r)

r, heard = run(home, 'auto, saved', :already_saved, action: 'auto')
check.call('automatic save of a saved device reports it saved',
           r['action'] == 'auto' && r['unsaved_before'] == 'false' && r['failure'].to_s.empty?, r)
check.call('automatic save never writes to a saved device', !heard.include?('save'), heard)

r, = run(home, 'auto, silent', :silent, action: 'auto')
check.call('an automatic save the device never confirms is not reported as saved', r['failure'] == 'no_answer', r)

puts failures.empty? ? 'ALL PASS' : "#{failures.size} FAILED"
exit(failures.empty? ? 0 : 1)
