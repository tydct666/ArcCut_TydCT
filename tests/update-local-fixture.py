"""Local TLS certificate and portable test ZIP. No system certificate changes."""
import sys,json,zipfile,hashlib,datetime,ipaddress
from pathlib import Path
from cryptography import x509
from cryptography.x509.oid import NameOID
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.hazmat.primitives import hashes,serialization
root=Path(sys.argv[2]).resolve()
if sys.argv[1]=='tls':
 root.mkdir(parents=True,exist_ok=True)
 key=rsa.generate_private_key(public_exponent=65537,key_size=2048)
 name=x509.Name([x509.NameAttribute(NameOID.COMMON_NAME,'ArkCut local test only')])
 now=datetime.datetime.now(datetime.timezone.utc)
 cert=x509.CertificateBuilder().subject_name(name).issuer_name(name).public_key(key.public_key()).serial_number(x509.random_serial_number()).not_valid_before(now-datetime.timedelta(days=1)).not_valid_after(now+datetime.timedelta(days=7)).add_extension(x509.SubjectAlternativeName([x509.DNSName('localhost'),x509.IPAddress(ipaddress.ip_address('127.0.0.1'))]),critical=False).add_extension(x509.BasicConstraints(ca=True,path_length=None),critical=True).sign(key,hashes.SHA256())
 (root/'tls-test.key').write_bytes(key.private_bytes(serialization.Encoding.PEM,serialization.PrivateFormat.PKCS8,serialization.NoEncryption()))
 (root/'tls-test.crt').write_bytes(cert.public_bytes(serialization.Encoding.PEM))
elif sys.argv[1]=='pack':
 manifest=json.loads((root/'manifest.json').read_text(encoding='utf-8-sig'))
 manifest['files']=[]
 for f in sorted(root.rglob('*')):
  if f.is_file() and f.relative_to(root).as_posix()!='manifest.json':manifest['files'].append(dict(path=f.relative_to(root).as_posix(),bytes=f.stat().st_size,sha256=hashlib.file_digest(f.open('rb'),'sha256').hexdigest()))
 (root/'manifest.json').write_text(json.dumps(manifest,ensure_ascii=False,indent=2),encoding='utf8')
 if len(sys.argv)>3:
  with zipfile.ZipFile(sys.argv[3],'w',zipfile.ZIP_DEFLATED,compresslevel=1)as z:
   for f in root.rglob('*'):
    if f.is_file():z.write(f,root.name+'/'+f.relative_to(root).as_posix())
 print(json.dumps(dict(version=manifest['version'],files=len(manifest['files']))))
