require_relative 'check'

class ReviewerMonitorTest
  def assert_empty(value); raise 'expected empty result' unless value.empty?; end
  def assert_equal(expected, actual); raise "expected #{expected.inspect}, got #{actual.inspect}" unless expected == actual; end
  def assert_raises(type)
    begin
      yield
    rescue type
      return
    end
    raise "expected #{type}"
  end
  def test_only_new_apple_messages_are_pending_and_edits_are_detected
    apple={'id'=>'1','actor_type'=>'APPLE','body_html'=>'old'}
    developer={'id'=>'2','actor_type'=>'DEVELOPER','body_html'=>'own reply'}
    unknown={'id'=>'3','body_html'=>'unknown actor'}
    state={'seen'=>{'1'=>Digest::SHA256.hexdigest(JSON.generate(apple))}}
    assert_empty ReviewerMonitor.pending([apple,developer,unknown],state)
    changed=apple.merge('body_html'=>'updated')
    assert_equal ['1'],ReviewerMonitor.pending([changed,developer],state).map { |m| m['id'] }
  end
  def test_malicious_pagination_is_rejected_before_second_request
    fake=Object.new
    calls=0
    fake.define_singleton_method(:request) do |*|
      calls+=1
      Struct.new(:status,:body).new(200,{'data'=>[], 'links'=>{'next'=>'https://example.com/iris/v1/resolutionCenterThreads'}})
    end
    assert_raises(ReviewerMonitor::Failure){ReviewerMonitor.pages(fake,'resolutionCenterThreads')}
    assert_equal 1,calls
  end
  def test_paginated_messages_are_all_returned
    fake=Object.new
    calls=0
    fake.define_singleton_method(:request) do |*|
      calls+=1
      Struct.new(:status,:body).new(200,{'data'=>[{'id'=>calls.to_s}], 'links'=>{'next'=>calls==1 ? 'https://appstoreconnect.apple.com/iris/v1/threads?cursor=2' : nil}})
    end
    assert_equal ['1','2'],ReviewerMonitor.pages(fake,'threads')['data'].map { |m| m['id'] }
  end
end

tests = ReviewerMonitorTest.new
methods = tests.methods.grep(/^test_/).sort
methods.each { |name| tests.public_send(name) }
puts "#{methods.length} reviewer monitor tests passed"
