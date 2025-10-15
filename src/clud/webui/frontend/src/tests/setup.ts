import '@testing-library/jest-dom';
import { vi } from 'vitest';

// Mock localStorage with a proper implementation
const storage: Record<string, string> = {};
const localStorageMock = {
	getItem: vi.fn((key: string) => storage[key] || null),
	setItem: vi.fn((key: string, value: string) => {
		storage[key] = value;
	}),
	removeItem: vi.fn((key: string) => {
		delete storage[key];
	}),
	clear: vi.fn(() => {
		Object.keys(storage).forEach(key => delete storage[key]);
	})
};
global.localStorage = localStorageMock as any;

// Mock WebSocket
global.WebSocket = vi.fn(() => ({
	close: vi.fn(),
	send: vi.fn(),
	addEventListener: vi.fn(),
	removeEventListener: vi.fn(),
	readyState: 1
})) as any;

// Mock ResizeObserver
global.ResizeObserver = vi.fn(() => ({
	observe: vi.fn(),
	unobserve: vi.fn(),
	disconnect: vi.fn()
})) as any;
