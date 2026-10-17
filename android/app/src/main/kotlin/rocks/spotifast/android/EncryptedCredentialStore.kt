package rocks.spotifast.android

import android.content.Context
import android.util.Base64
import androidx.security.crypto.EncryptedSharedPreferences
import androidx.security.crypto.MasterKey
import uniffi.spotifast.CredentialStore

/**
 * The Android `ProtectedStore` arm: keeps grants in `EncryptedSharedPreferences`
 * (Android-Keystore-backed), keyed as the core expects. Values are Base64 so
 * arbitrary bytes survive the string store.
 */
class EncryptedCredentialStore(context: Context) : CredentialStore {

    private val preferences = run {
        val masterKey = MasterKey.Builder(context)
            .setKeyScheme(MasterKey.KeyScheme.AES256_GCM)
            .build()
        EncryptedSharedPreferences.create(
            context,
            "spotifast-credentials",
            masterKey,
            EncryptedSharedPreferences.PrefKeyEncryptionScheme.AES256_SIV,
            EncryptedSharedPreferences.PrefValueEncryptionScheme.AES256_GCM,
        )
    }

    override fun read(key: String): ByteArray? {
        val stored = preferences.getString(key, null) ?: return null
        return Base64.decode(stored, Base64.NO_WRAP)
    }

    override fun write(key: String, value: ByteArray): Boolean {
        return try {
            preferences.edit()
                .putString(key, Base64.encodeToString(value, Base64.NO_WRAP))
                .apply()
            true
        } catch (_: Exception) {
            false
        }
    }

    override fun delete(key: String): Boolean {
        return try {
            preferences.edit().remove(key).apply()
            true
        } catch (_: Exception) {
            false
        }
    }
}
