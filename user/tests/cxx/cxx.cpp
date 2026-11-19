// A C++ program for the tests of the C++ runtimes, built linked to
// libc++.dlib and linked statically. It checks exceptions, also across a
// thread, run-time type information, threads with a mutex and a
// condition variable, string streams, std::filesystem and static
// initialization, and exits with 0 when every check passed, and
// otherwise with the number of the first check that failed.

#include <atomic>
#include <condition_variable>
#include <exception>
#include <filesystem>
#include <memory>
#include <mutex>
#include <sstream>
#include <stdexcept>
#include <string>
#include <thread>
#include <typeinfo>
#include <vector>

namespace {

struct Base {
	virtual ~Base() = default;
};

struct Derived : Base {};

// Set before main runs.
int initialized = [] { return 42; }();

[[noreturn]] void fail_deeply(int depth)
{
	if (depth == 0)
		throw std::runtime_error("deep");
	fail_deeply(depth - 1);
}

} // namespace

int main()
{
	// Exceptions, thrown through frames and caught by base class.
	try {
		fail_deeply(16);
	} catch (const std::exception &e) {
		if (std::string(e.what()) != "deep")
			return 10;
	}
	try {
		try {
			throw 5;
		} catch (int) {
			throw;
		}
	} catch (int v) {
		if (v != 5)
			return 11;
	}

	// Run-time type information.
	std::unique_ptr<Base> object = std::make_unique<Derived>();
	const Base &base = *object;
	if (!dynamic_cast<const Derived *>(&base) || typeid(base) != typeid(Derived))
		return 20;

	// Threads that report to the main one.
	std::mutex lock;
	std::condition_variable changed;
	int done = 0;
	std::atomic<int> sum{0};
	std::vector<std::thread> threads;
	for (int i = 0; i < 4; i++) {
		threads.emplace_back([&, i] {
			sum += i;
			std::lock_guard<std::mutex> guard(lock);
			done++;
			changed.notify_one();
		});
	}
	{
		std::unique_lock<std::mutex> guard(lock);
		changed.wait(guard, [&] { return done == 4; });
	}
	for (auto &t : threads)
		t.join();
	if (sum != 6)
		return 30;
	// An exception thrown in a thread, caught in another.
	std::exception_ptr thrown;
	std::thread([&] {
		try {
			throw std::logic_error("across");
		} catch (...) {
			thrown = std::current_exception();
		}
	}).join();
	try {
		std::rethrow_exception(thrown);
	} catch (const std::logic_error &e) {
		if (std::string(e.what()) != "across")
			return 31;
	}

	// Formatting.
	std::ostringstream text;
	text << "x=" << 3.5 << ' ' << 12;
	if (text.str() != "x=3.5 12")
		return 40;

	// Files.
	if (!std::filesystem::is_directory("/system/bin"))
		return 50;
	int entries = 0;
	for (const auto &entry : std::filesystem::directory_iterator("/system/bin")) {
		(void)entry;
		entries++;
	}
	if (entries == 0)
		return 51;

	if (initialized != 42)
		return 60;
	return 0;
}
