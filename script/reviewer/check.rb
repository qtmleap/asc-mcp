#!/usr/bin/env ruby
# Read-only App Review monitor using fastlane Spaceship, without browser control.
require 'json'
require 'digest'
require 'fileutils'
require 'uri'
require 'timeout'
require 'spaceship'
require 'http/cookie_jar'
require 'logger'
require 'time'

module ReviewerMonitor
  ORIGIN = 'https://appstoreconnect.apple.com'
  ROOT = File.expand_path('~/Library/Application Support/InterceptorReview')
  class Failure < StandardError; end
  class SessionClient < Spaceship::Client
    def self.hostname; 'https://appstoreconnect.apple.com/'; end
  end

  def self.json_get(client, path, query = {})
    response = client.request(:get, "#{ORIGIN}/iris/v1/#{path}", query)
    raise Failure, 'authentication_required' if [401, 403].include?(response.status) || (300...400).cover?(response.status)
    raise Failure, "api_http_#{response.status}" unless (200...300).cover?(response.status)
    value = response.body.is_a?(Hash) ? response.body : JSON.parse(response.body)
    raise Failure, 'api_error_response' unless value.is_a?(Hash) && value['data'].is_a?(Array)
    value
  end

  def self.pages(client, path, query = {})
    all = []; included = []; seen = {}
    20.times do
      response = json_get(client, path, query)
      all.concat(response.fetch('data')); included.concat(response.fetch('included', []))
      next_url = response.dig('links', 'next')
      return {'data'=>all, 'included'=>included} if next_url.nil? || next_url.empty?
      raise Failure, 'pagination_cycle' if seen[next_url]
      seen[next_url] = true
      parsed = URI(next_url)
      expected = "/iris/v1/#{path}"
      raise Failure, 'unsafe_pagination_url' unless parsed.scheme == 'https' && parsed.host == 'appstoreconnect.apple.com' && parsed.port == 443 && parsed.userinfo.nil? && parsed.fragment.nil? && parsed.path == expected
      query = URI.decode_www_form(parsed.query || '').to_h
    end
    raise Failure, 'pagination_incomplete'
  end

  def self.client(cookie_path)
    jar = HTTP::CookieJar.new
    jar.load(cookie_path)
    instance = SessionClient.new(cookie: jar, timeout: 12)
    # Do not persist network bodies, cookies, or headers in fastlane's logs.
    def instance.log_request(*); end
    def instance.log_response(*); end
    def instance.logger; @quiet_logger ||= Logger.new(File::NULL); end
    instance
  end

  def self.read(app_id, cookie_path)
    c = client(cookie_path)
    submissions = pages(c, "apps/#{app_id}/reviewSubmissions", {'limit'=>'200'})['data']
    output = []
    submissions.each do |submission|
      threads = pages(c, 'resolutionCenterThreads', {'filter[reviewSubmission]'=>submission.fetch('id'), 'limit'=>'200'})['data']
      threads.each do |thread|
        messages = pages(c, "resolutionCenterThreads/#{thread.fetch('id')}/resolutionCenterMessages", {'include'=>'fromActor,rejections,resolutionCenterMessageAttachments', 'limit'=>'200'})
        actors = messages['included'].select { |r| r['type'] == 'actors' }.to_h { |r| [r['id'], r.fetch('attributes', {})] }
        messages['data'].each do |message|
          attrs = message.fetch('attributes', {})
          actor_id = message.dig('relationships', 'fromActor', 'data', 'id')
          actor = actors.fetch(actor_id, {})
          output << {'id'=>message['id'], 'thread_id'=>thread['id'], 'submission_id'=>submission['id'], 'created_date'=>attrs['createdDate'], 'actor_type'=>actor['actorType'], 'actor_name'=>actor['name'], 'body_html'=>attrs['messageBody'], 'rejection_references'=>message.dig('relationships', 'rejections', 'data')}
        end
      end
    end
    [output, c]
  end

  def self.pending(messages, state)
    known = state.fetch('seen', {})
    messages.filter_map do |message|
      # Only notify for a positively identified Apple actor; do not mistake the
      # developer's own reply or an unknown actor for a new reviewer response.
      next unless message['actor_type'] == 'APPLE'
      digest = Digest::SHA256.hexdigest(JSON.generate(message))
      message.merge('fingerprint'=>digest) unless known[message.fetch('id')] == digest
    end
  end

  def self.atomic_json(path, object)
    temporary = "#{path}.tmp-#{Process.pid}"
    File.open(temporary, File::WRONLY|File::CREAT|File::TRUNC, 0600) { |f| f.write(JSON.pretty_generate(object)) }
    File.rename(temporary, path)
  end

  def self.run
    FileUtils.mkdir_p(ROOT, mode: 0700)
    state_path = File.join(ROOT, 'state.json')
    snapshot_path = File.join(ROOT, 'messages.json')
    state = File.exist?(state_path) ? JSON.parse(File.read(state_path)) : {'seen'=>{}}
    if ARGV.first == '--ack'
      raise Failure, 'no_snapshot_to_acknowledge' unless File.exist?(snapshot_path)
      snapshot = File.read(snapshot_path)
      raise Failure, 'snapshot_changed_or_missing_token' unless ARGV[1] && Digest::SHA256.hexdigest(snapshot) == ARGV[1]
      messages = JSON.parse(snapshot).fetch('messages')
      state['seen'] ||= {}
      messages.each { |m| state['seen'][m.fetch('id')] = Digest::SHA256.hexdigest(JSON.generate(m)) if m['actor_type'] == 'APPLE' }
      atomic_json(state_path, state)
      puts JSON.generate(status: 'acknowledged'); return
    end
    app_id = ENV.fetch('ASC_REVIEW_APP_ID', '6749347474')
    raise Failure, 'invalid_app_id' unless app_id.match?(/\A[0-9]+\z/)
    own_session = File.join(ROOT, 'session.cookie')
    paths = ENV['ASC_REVIEW_COOKIE_PATH'] ? [ENV['ASC_REVIEW_COOKIE_PATH']] : ([own_session].select { |p| File.exist?(p) } + Dir.glob(File.expand_path('~/.fastlane/spaceship/*/cookie')))
    paths = paths.select { |p| File.file?(p) }
    raise Failure, 'authentication_required' if paths.empty?
    messages = nil; last_failure = 'authentication_required'; successful_client = nil
    paths.each do |path|
      begin
        Timeout.timeout(55) { messages, successful_client = read(app_id, path) }
        break
      rescue StandardError => e
        # Never print authentication headers, cookies, raw response bodies or
        # exception text containing them. Existing sessions are read-only.
        warn JSON.generate(error_class: e.class.name, line: e.backtrace.first&.split(':in')&.first) if ENV['ASC_REVIEW_DIAGNOSTIC'] == '1'
        last_failure = if e.is_a?(Spaceship::UnauthorizedAccessError)
          'authentication_required'
        elsif e.is_a?(Failure)
          e.message
        else
          'check_failed'
        end
      end
    end
    raise Failure, last_failure unless messages
    atomic_json(snapshot_path, {'app_id'=>app_id, 'messages'=>messages, 'checked_at'=>Time.now.utc.iso8601})
    session_path = File.join(ROOT, 'session.cookie')
    successful_client.instance_variable_get(:@cookie).save(session_path, :yaml, session: true)
    File.chmod(0600, session_path)
    changes = pending(messages, state)
    puts JSON.generate(status: changes.empty? ? 'unchanged' : 'new_messages', app_id: app_id, snapshot_path: snapshot_path, snapshot_token: Digest::SHA256.file(snapshot_path).hexdigest, new_messages: changes)
  rescue Failure => e
    puts JSON.generate(status: 'blocked', reason: e.message)
    exit 2
  rescue StandardError
    puts JSON.generate(status: 'blocked', reason: 'check_failed')
    exit 2
  end
end

if $PROGRAM_NAME == __FILE__
  if ARGV.first == '--login'
    FileUtils.mkdir_p(ReviewerMonitor::ROOT, mode: 0700)
    # CLI login only. Never perform login or prompt for 2FA during a scheduled
    # check or an MCP call, and never print/export the resulting cookie jar.
    class Spaceship::Client
      def log_request(*); end
      def log_response(*); end
    end
    email = ARGV[1]
    unless email
      print 'Apple ID: '
      email = STDIN.gets&.strip
    end
    abort 'Apple ID is required' if email.nil? || email.empty?
    client = Spaceship::Tunes.login(email)
    path = File.join(ReviewerMonitor::ROOT, 'session.cookie')
    old_umask = File.umask(0077)
    begin
      client.instance_variable_get(:@cookie).save(path, :yaml, session: true)
      File.chmod(0600, path)
    ensure
      File.umask(old_umask)
    end
    puts 'Authentication saved locally. Run check.sh to verify App Review access.'
  else
    ReviewerMonitor.run
  end
end
