package krusty

import org.gradle.api.GradleException
import kotlin.test.Test
import kotlin.test.assertEquals
import kotlin.test.assertFailsWith

class ReleaseSignatureTest {
    private val what = "SHA256SUMS of the krusty test release"

    private fun fixture(name: String): String =
        checkNotNull(javaClass.classLoader.getResourceAsStream("krusty/signatures/$name")) { name }
            .use { String(it.readBytes(), Charsets.UTF_8) }

    private val sums = fixture("SHA256SUMS").toByteArray()
    private val gpgRoot = ReleaseRootKey(fixture("root-key.asc"))

    private fun rejection(block: () -> Unit): String? = assertFailsWith<GradleException>(block = block).message

    @Test
    fun theBundledRootIsTheMaintainersKey() {
        assertEquals(
            "d1a776abf83dbe95d365c2a1b8713034dfaeaa99",
            ReleaseRootKey.bundled().fingerprint.joinToString("") { "%02x".format(it) },
        )
    }

    @Test
    fun acceptsGnupgSignaturesByCertifiedSubkeys() {
        gpgRoot.verify(sums, fixture("SHA256SUMS.ed25519.asc"), fixture("ed25519-signing-key.asc"), what)
        gpgRoot.verify(sums, fixture("SHA256SUMS.rsa.asc"), fixture("rsa-signing-key.asc"), what)
        gpgRoot.verify(sums, fixture("SHA256SUMS.expiring.asc"), fixture("expiring-signing-key.asc"), what)
    }

    @Test
    fun rejectsChangedSums() {
        val changed = sums.copyOf().also { it[0] = 'f'.code.toByte() }

        assertEquals(
            "the signature of $what does not match it",
            rejection { gpgRoot.verify(changed, fixture("SHA256SUMS.ed25519.asc"), fixture("ed25519-signing-key.asc"), what) },
        )
    }

    @Test
    fun rejectsASignatureByAnotherSubkeyThanTheCertificateBinds() {
        assertEquals(
            "$what is not signed by a subkey of the krusty release root key",
            rejection { gpgRoot.verify(sums, fixture("SHA256SUMS.rsa.asc"), fixture("ed25519-signing-key.asc"), what) },
        )
    }

    @Test
    fun rejectsASubkeyOfAnotherRoot() {
        val other = TestReleaseSigner()

        assertEquals(
            "the signing key of $what does not belong to the krusty release root key",
            rejection { gpgRoot.verify(sums, other.sign(sums), other.certificate, what) },
        )
    }

    @Test
    fun rejectsACertificateWhoseBindingTheRootDidNotSign() {
        val signer = TestReleaseSigner(forgeBinding = true)

        assertEquals(
            "$what is not signed by a subkey of the krusty release root key",
            rejection { signer.rootKey.verify(sums, signer.sign(sums), signer.certificate, what) },
        )
    }

    @Test
    fun rejectsASubkeyNotBoundForSigning() {
        val signer = TestReleaseSigner(keyFlags = 0x0c)

        assertEquals(
            "$what is not signed by a subkey of the krusty release root key",
            rejection { signer.rootKey.verify(sums, signer.sign(sums), signer.certificate, what) },
        )
    }

    @Test
    fun acceptsASignatureMadeBeforeTheSubkeyExpired() {
        val signer = TestReleaseSigner(subkeyCreated = 1_000_000, keyExpiration = 100)

        signer.rootKey.verify(sums, signer.sign(sums, time = 1_000_099), signer.certificate, what)
    }

    @Test
    fun rejectsASignatureMadeAfterTheSubkeyExpired() {
        val signer = TestReleaseSigner(subkeyCreated = 1_000_000, keyExpiration = 100)

        assertEquals(
            "$what was signed outside the validity of its signing subkey",
            rejection { signer.rootKey.verify(sums, signer.sign(sums, time = 1_000_100), signer.certificate, what) },
        )
    }

    @Test
    fun rejectsASignatureMadeBeforeTheSubkeyExisted() {
        val signer = TestReleaseSigner(subkeyCreated = 1_000_000)

        assertEquals(
            "$what was signed outside the validity of its signing subkey",
            rejection { signer.rootKey.verify(sums, signer.sign(sums, time = 999_999), signer.certificate, what) },
        )
    }

    @Test
    fun rejectsAnArmoredBlockThatIsNotASignature() {
        assertEquals(
            "not an armored OpenPGP SIGNATURE",
            rejection { gpgRoot.verify(sums, fixture("root-key.asc"), fixture("ed25519-signing-key.asc"), what) },
        )
    }
}
