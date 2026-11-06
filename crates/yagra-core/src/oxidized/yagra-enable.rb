# Yagra's replacement for oxidized's `enable` macro (ADR-210 stage 7, decisions 69-71).
#
# Written by core into oxidized's home beside the configuration, and loaded by the model files core
# writes under `model/` (`ios.rb`, `tplink.rb`) before they load oxidized's own model of that name:
# oxidized reads a model from its home before its own directory (`manager.rb`, `loader`).
#
# oxidized's macro sends `enable` whenever the login carries an enable password and then waits for
# a password question. A device whose login lands it in privileged mode never asks: it answers with
# its `#` prompt, the wait runs out after the timeout, and the fetch - and the save that only runs
# after a fetch - fails every time (measured on a Catalyst 9200, IOS-XE 17.3, 2026-11-06).
#
# This sends an empty line first and reads the prompt it gets back. A prompt ending in `#` is
# already privileged, so nothing more is sent; anything else is handled exactly as oxidized does.
# The one line added is the empty one.
#
# 🚨 This leans on oxidized internals: the private `macro_enable` in `Oxidized::Model::Macros` and
# its keyword arguments, `cfg`, `post_login`, and `cmd` and `vars` on the input a post_login block
# runs against. `scripts/oxidized-script-test.sh` drives it in the pinned image, against a device
# that lands in `#` and one that lands in `>`; an oxidized upgrade that moves any of them fails there.

require 'oxidized'

module Oxidized
  class Model
    module Macros
      # Whether the prompt a device answered with is a privileged one.
      def self.yagra_privileged?(answer)
        answer.to_s.rstrip.end_with?('#')
      end

      private

      def macro_enable(regex: /password/i, inputs: %i[telnet ssh], command: 'enable')
        inputs = [inputs].flatten.map(&:to_sym)
        cfg(*inputs) do
          post_login do
            next unless vars(:enable)
            next if Oxidized::Model::Macros.yagra_privileged?(cmd(''))

            if vars(:enable) == true
              cmd command
            else
              cmd command, regex
              cmd vars(:enable)
            end
          end
        end
      end
    end
  end
end
