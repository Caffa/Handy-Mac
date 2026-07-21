/**
 * Audio testing helpers.
 * These can generate test audio files and feed them to the app.
 */
import { execSync } from 'child_process';
import * as path from 'path';
import * as fs from 'fs';

const FIXTURES_DIR = path.resolve(__dirname, '../fixtures');

/**
 * Get the absolute path to a test audio fixture file.
 * Throws if the file doesn't exist — run generateTestAudio() first.
 */
export function getTestAudioPath(name: string = 'test-sine-2s.wav'): string {
  const filePath = path.join(FIXTURES_DIR, name);
  if (!fs.existsSync(filePath)) {
    throw new Error(
      `Test audio file not found: ${filePath}. Run 'bun run test:e2e:generate-fixtures' first.`,
    );
  }
  return filePath;
}

/**
 * Generate a 2-second 440Hz sine wave at 16kHz mono (Whisper-compatible format).
 * Requires ffmpeg to be available on PATH.
 */
export function generateTestAudio(): void {
  fs.mkdirSync(FIXTURES_DIR, { recursive: true });

  const outputPath = path.join(FIXTURES_DIR, 'test-sine-2s.wav');

  // Generate 2-second 440Hz sine wave at 16kHz mono
  execSync(
    `ffmpeg -f lavfi -i "sine=frequency=440:duration=2" -ar 16000 -ac 1 -y "${outputPath}" 2>/dev/null`,
    { stdio: 'ignore' },
  );
}