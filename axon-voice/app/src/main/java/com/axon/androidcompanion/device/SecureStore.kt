package com.axon.androidcompanion.device

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * AES-256/GCM encryption backed by a key that never leaves the AndroidKeyStore.
 *
 * Used instead of androidx.security:security-crypto — that library was deprecated
 * by Google in April 2025 without ever shipping a stable 1.1.0 release, so this
 * hand-rolled Keystore usage (a long-stable, non-deprecated framework API) avoids
 * depending on an abandoned alpha artifact for something as sensitive as auth tokens.
 */
object SecureStore {

    private const val KEYSTORE       = "AndroidKeyStore"
    private const val KEY_ALIAS      = "androidcompanion_secret_key"
    private const val TRANSFORMATION = "AES/GCM/NoPadding"
    private const val GCM_TAG_BITS   = 128
    private const val GCM_IV_BYTES   = 12

    private fun getOrCreateKey(): SecretKey {
        val ks = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (ks.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }

        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(
                KEY_ALIAS,
                KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT
            )
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build()
        )
        return generator.generateKey()
    }

    /** Encrypts [plaintext]; returns a Base64 string (IV || ciphertext) safe to store in SharedPreferences. */
    fun encrypt(plaintext: String): String {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, getOrCreateKey())
        val iv         = cipher.iv
        val ciphertext = cipher.doFinal(plaintext.toByteArray(Charsets.UTF_8))
        return Base64.encodeToString(iv + ciphertext, Base64.NO_WRAP)
    }

    /** Reverses [encrypt]. Returns null if [blob] is missing/corrupt or the Keystore key was reset. */
    fun decrypt(blob: String): String? = try {
        val raw        = Base64.decode(blob, Base64.NO_WRAP)
        val iv         = raw.copyOfRange(0, GCM_IV_BYTES)
        val ciphertext = raw.copyOfRange(GCM_IV_BYTES, raw.size)
        val cipher     = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, getOrCreateKey(), GCMParameterSpec(GCM_TAG_BITS, iv))
        String(cipher.doFinal(ciphertext), Charsets.UTF_8)
    } catch (e: Exception) {
        null
    }
}
